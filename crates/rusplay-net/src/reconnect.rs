use std::{
    collections::VecDeque,
    fmt,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use hkdf::{
    Hkdf,
    hmac::{Hmac, KeyInit, Mac},
};
use parking_lot::RwLock;
use rusplay_config::NetworkConfig;
use rusplay_protocol::{PeerId, ReconnectMessage, ReconnectRejection};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::{
    NetError, Transport,
    pairing::fill_random,
    peers::{PeerRecord, PeerSecret, PeerStore},
    session::{KEY_LEN, SessionKeys},
};

const LINGER_RETRANSMITS: u32 = 3;
const RECEIVE_BUFFER_LEN: usize = 2048;
const MAX_REMEMBERED_ACCEPTANCES: usize = 64;
const COOKIE_KEY_LEN: usize = 32;
const HMAC_BLOCK_LEN: usize = 64;
const BLIND_LABEL: &[u8] = b"rusplay reconnect blind";
const COOKIE_LABEL: &[u8] = b"rusplay reconnect cookie";

type Nonce = [u8; ReconnectMessage::NONCE_LEN];
type Blind = [u8; ReconnectMessage::BLIND_LEN];
type Cookie = [u8; ReconnectMessage::COOKIE_LEN];
type Proof = [u8; ReconnectMessage::PROOF_LEN];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectTiming {
    pub retransmit: Duration,
    pub timeout: Duration,
}

impl Default for ReconnectTiming {
    fn default() -> Self {
        Self::from(&NetworkConfig::default())
    }
}

impl From<&NetworkConfig> for ReconnectTiming {
    fn from(config: &NetworkConfig) -> Self {
        Self {
            retransmit: Duration::from_millis(u64::from(config.control_retransmit_ms)),
            timeout: Duration::from_millis(u64::from(config.reconnect_timeout_ms)),
        }
    }
}

fn random_nonce() -> Result<Nonce, NetError> {
    let mut nonce = [0; ReconnectMessage::NONCE_LEN];
    fill_random(&mut nonce)?;
    Ok(nonce)
}

fn random_attempt() -> Result<u32, NetError> {
    let mut bytes = [0; 4];
    fill_random(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn tags_match(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.ct_eq(b).into()
}

fn keyed_hash(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut block = [0; HMAC_BLOCK_LEN];
    block[..key.len()].copy_from_slice(key);
    let mut mac = Hmac::<Sha256>::new(&block.into());
    block.zeroize();

    for part in parts {
        mac.update(part);
    }

    mac.finalize().into_bytes().into()
}

fn blind_for(secret: &PeerSecret, attempt: u32, nonce: &Nonce) -> Blind {
    keyed_hash(
        secret.as_bytes(),
        &[BLIND_LABEL, &attempt.to_be_bytes(), nonce],
    )
}

fn blinded_record<'a>(
    candidates: impl IntoIterator<Item = &'a PeerRecord>,
    attempt: u32,
    nonce: &Nonce,
    blind: &Blind,
) -> Option<&'a PeerRecord> {
    candidates
        .into_iter()
        .find(|record| tags_match(&blind_for(record.secret(), attempt, nonce), blind))
}

pub(crate) fn is_blinded_by(record: &PeerRecord, message: &ReconnectMessage) -> bool {
    match message {
        ReconnectMessage::Hello {
            attempt,
            nonce,
            blind,
        }
        | ReconnectMessage::Proof {
            attempt,
            nonce,
            blind,
            ..
        } => blinded_record([record], *attempt, nonce, blind).is_some(),
        _ => false,
    }
}

struct Schedule {
    host_proof: Proof,
    client_proof: Proof,
    client_to_host: [u8; KEY_LEN],
    host_to_client: [u8; KEY_LEN],
}

impl Drop for Schedule {
    fn drop(&mut self) {
        self.host_proof.zeroize();
        self.client_proof.zeroize();
        self.client_to_host.zeroize();
        self.host_to_client.zeroize();
    }
}

impl Schedule {
    fn derive(
        secret: &PeerSecret,
        peer: &PeerId,
        attempt: u32,
        client_nonce: &Nonce,
        cookie: &Cookie,
    ) -> Result<Self, NetError> {
        let mut salt = Vec::with_capacity(
            PeerId::LEN + 4 + ReconnectMessage::NONCE_LEN + ReconnectMessage::COOKIE_LEN,
        );
        salt.extend_from_slice(&peer.0);
        salt.extend_from_slice(&attempt.to_be_bytes());
        salt.extend_from_slice(client_nonce);
        salt.extend_from_slice(cookie);

        let hkdf = Hkdf::<Sha256>::new(Some(&salt), secret.as_bytes());
        let expand = |label: &[u8]| -> Result<[u8; KEY_LEN], NetError> {
            let mut out = [0; KEY_LEN];
            hkdf.expand(label, &mut out)
                .map_err(NetError::ReconnectKeyDerivation)?;
            Ok(out)
        };

        Ok(Self {
            host_proof: expand(b"rusplay reconnect host proof")?,
            client_proof: expand(b"rusplay reconnect client proof")?,
            client_to_host: expand(b"rusplay reconnect client to host")?,
            host_to_client: expand(b"rusplay reconnect host to client")?,
        })
    }

    fn client_keys(&self) -> SessionKeys {
        SessionKeys::new(self.client_to_host, self.host_to_client)
    }

    fn host_keys(&self) -> SessionKeys {
        SessionKeys::new(self.host_to_client, self.client_to_host)
    }
}

#[derive(Debug)]
pub enum ReconnectStep {
    Pending,
    Complete(SessionKeys),
}

enum InitiatorStage {
    AwaitChallenge,
    AwaitAccepted { proof: Vec<u8>, keys: SessionKeys },
    Finished,
}

pub struct ReconnectInitiator {
    attempt: u32,
    peer: PeerId,
    secret: PeerSecret,
    client_nonce: Nonce,
    blind: Blind,
    hello: Vec<u8>,
    stage: InitiatorStage,
}

impl fmt::Debug for ReconnectInitiator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconnectInitiator")
            .field("attempt", &self.attempt)
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl ReconnectInitiator {
    pub fn new(record: &PeerRecord) -> Result<Self, NetError> {
        let attempt = random_attempt()?;
        let client_nonce = random_nonce()?;
        let blind = blind_for(record.secret(), attempt, &client_nonce);
        let hello = ReconnectMessage::Hello {
            attempt,
            nonce: client_nonce,
            blind,
        }
        .to_datagram()?;

        Ok(Self {
            attempt,
            peer: record.id(),
            secret: record.secret().clone(),
            client_nonce,
            blind,
            hello,
            stage: InitiatorStage::AwaitChallenge,
        })
    }

    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn outgoing(&self) -> Option<&[u8]> {
        match &self.stage {
            InitiatorStage::AwaitChallenge => Some(&self.hello),
            InitiatorStage::AwaitAccepted { proof, .. } => Some(proof),
            InitiatorStage::Finished => None,
        }
    }

    pub fn handle(&mut self, datagram: &[u8]) -> Result<ReconnectStep, NetError> {
        let message = ReconnectMessage::from_datagram(datagram)?;
        if message.attempt() != self.attempt {
            return Ok(ReconnectStep::Pending);
        }

        if let ReconnectMessage::Rejected { reason, .. } = message {
            self.stage = InitiatorStage::Finished;
            return Err(NetError::ReconnectRejected { reason });
        }

        match (
            std::mem::replace(&mut self.stage, InitiatorStage::Finished),
            message,
        ) {
            (InitiatorStage::AwaitChallenge, ReconnectMessage::Challenge { cookie, proof, .. }) => {
                self.accept_challenge(&cookie, &proof)
            }
            (InitiatorStage::AwaitAccepted { keys, .. }, ReconnectMessage::Accepted { .. }) => {
                Ok(ReconnectStep::Complete(keys))
            }
            (stage, _) => {
                self.stage = stage;
                Ok(ReconnectStep::Pending)
            }
        }
    }

    fn accept_challenge(
        &mut self,
        cookie: &Cookie,
        host_proof: &Proof,
    ) -> Result<ReconnectStep, NetError> {
        let schedule = Schedule::derive(
            &self.secret,
            &self.peer,
            self.attempt,
            &self.client_nonce,
            cookie,
        )?;

        if !tags_match(&schedule.host_proof, host_proof) {
            return Err(NetError::ReconnectAuthentication);
        }

        let proof = ReconnectMessage::Proof {
            attempt: self.attempt,
            nonce: self.client_nonce,
            blind: self.blind,
            cookie: *cookie,
            proof: schedule.client_proof,
        }
        .to_datagram()?;

        self.stage = InitiatorStage::AwaitAccepted {
            proof,
            keys: schedule.client_keys(),
        };
        Ok(ReconnectStep::Pending)
    }
}

struct CookieKey([u8; COOKIE_KEY_LEN]);

impl Drop for CookieKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

struct Acceptance {
    attempt: u32,
    cookie: Cookie,
    origin: SocketAddr,
    expires: Instant,
    reply: Vec<u8>,
}

#[derive(Debug)]
pub struct ReconnectAccepted {
    pub peer: PeerId,
    pub keys: SessionKeys,
}

#[derive(Debug, Default)]
pub struct ReconnectOutput {
    pub reply: Option<Vec<u8>>,
    pub accepted: Option<ReconnectAccepted>,
}

pub struct ReconnectResponder {
    key: CookieKey,
    epoch: Instant,
    cookie_period: Duration,
    acceptances: VecDeque<Acceptance>,
}

impl fmt::Debug for ReconnectResponder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconnectResponder")
            .field("cookie_period", &self.cookie_period)
            .field("acceptances", &self.acceptances.len())
            .finish_non_exhaustive()
    }
}

impl ReconnectResponder {
    pub fn new(timing: ReconnectTiming) -> Result<Self, NetError> {
        let mut key = CookieKey([0; COOKIE_KEY_LEN]);
        fill_random(&mut key.0)?;

        Ok(Self {
            key,
            epoch: Instant::now(),
            cookie_period: timing.timeout.max(Duration::from_millis(1)),
            acceptances: VecDeque::new(),
        })
    }

    pub fn handle(
        &mut self,
        datagram: &[u8],
        from: SocketAddr,
        store: &PeerStore,
    ) -> Result<ReconnectOutput, NetError> {
        let message = ReconnectMessage::from_datagram(datagram)?;
        self.respond(&message, from, store.iter(), Instant::now())
    }

    pub(crate) fn respond<'a>(
        &mut self,
        message: &ReconnectMessage,
        from: SocketAddr,
        candidates: impl IntoIterator<Item = &'a PeerRecord>,
        now: Instant,
    ) -> Result<ReconnectOutput, NetError> {
        match *message {
            ReconnectMessage::Hello {
                attempt,
                nonce,
                blind,
            } => self.on_hello(attempt, &nonce, &blind, from, candidates, now),
            ReconnectMessage::Proof {
                attempt,
                nonce,
                blind,
                cookie,
                proof,
            } => self.on_proof(
                &Presented {
                    attempt,
                    nonce,
                    blind,
                    cookie,
                    proof,
                },
                from,
                candidates,
                now,
            ),
            _ => Ok(ReconnectOutput::default()),
        }
    }

    fn period_index(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.epoch).as_nanos();
        u64::try_from(elapsed / self.cookie_period.as_nanos()).unwrap_or(u64::MAX)
    }

    fn cookie(
        &self,
        period: u64,
        from: SocketAddr,
        attempt: u32,
        nonce: &Nonce,
        blind: &Blind,
    ) -> Cookie {
        let (family, address) = match from.ip() {
            IpAddr::V4(ip) => (4, ip.to_ipv6_mapped().octets()),
            IpAddr::V6(ip) => (6, ip.octets()),
        };

        keyed_hash(
            &self.key.0,
            &[
                COOKIE_LABEL,
                &period.to_be_bytes(),
                &[family],
                &address,
                &from.port().to_be_bytes(),
                &attempt.to_be_bytes(),
                nonce,
                blind,
            ],
        )
    }

    fn cookie_is_fresh(&self, presented: &Presented, from: SocketAddr, now: Instant) -> bool {
        let current = self.period_index(now);

        [Some(current), current.checked_sub(1)]
            .into_iter()
            .flatten()
            .any(|period| {
                let expected = self.cookie(
                    period,
                    from,
                    presented.attempt,
                    &presented.nonce,
                    &presented.blind,
                );
                tags_match(&expected, &presented.cookie)
            })
    }

    fn on_hello<'a>(
        &self,
        attempt: u32,
        nonce: &Nonce,
        blind: &Blind,
        from: SocketAddr,
        candidates: impl IntoIterator<Item = &'a PeerRecord>,
        now: Instant,
    ) -> Result<ReconnectOutput, NetError> {
        let Some(record) = blinded_record(candidates, attempt, nonce, blind) else {
            return rejection(attempt, ReconnectRejection::UnknownPeer);
        };

        let cookie = self.cookie(self.period_index(now), from, attempt, nonce, blind);
        let schedule = Schedule::derive(record.secret(), &record.id(), attempt, nonce, &cookie)?;

        Ok(reply_with(
            ReconnectMessage::Challenge {
                attempt,
                cookie,
                proof: schedule.host_proof,
            }
            .to_datagram()?,
        ))
    }

    fn on_proof<'a>(
        &mut self,
        presented: &Presented,
        from: SocketAddr,
        candidates: impl IntoIterator<Item = &'a PeerRecord>,
        now: Instant,
    ) -> Result<ReconnectOutput, NetError> {
        self.acceptances
            .retain(|acceptance| acceptance.expires > now);

        let attempt = presented.attempt;
        if let Some(acceptance) = self.acceptances.iter().find(|acceptance| {
            acceptance.attempt == attempt
                && acceptance.origin == from
                && tags_match(&acceptance.cookie, &presented.cookie)
        }) {
            return Ok(reply_with(acceptance.reply.clone()));
        }

        if !self.cookie_is_fresh(presented, from, now) {
            return Ok(ReconnectOutput::default());
        }

        let Some(record) = blinded_record(candidates, attempt, &presented.nonce, &presented.blind)
        else {
            return rejection(attempt, ReconnectRejection::UnknownPeer);
        };

        let peer = record.id();
        let schedule = Schedule::derive(
            record.secret(),
            &peer,
            attempt,
            &presented.nonce,
            &presented.cookie,
        )?;
        if !tags_match(&schedule.client_proof, &presented.proof) {
            return rejection(attempt, ReconnectRejection::Authentication);
        }

        let accepted = ReconnectMessage::Accepted { attempt }.to_datagram()?;
        if self.acceptances.len() >= MAX_REMEMBERED_ACCEPTANCES {
            self.acceptances.pop_front();
        }
        self.acceptances.push_back(Acceptance {
            attempt,
            cookie: presented.cookie,
            origin: from,
            expires: now + 2 * self.cookie_period,
            reply: accepted.clone(),
        });

        Ok(ReconnectOutput {
            reply: Some(accepted),
            accepted: Some(ReconnectAccepted {
                peer,
                keys: schedule.host_keys(),
            }),
        })
    }
}

struct Presented {
    attempt: u32,
    nonce: Nonce,
    blind: Blind,
    cookie: Cookie,
    proof: Proof,
}

fn reply_with(datagram: Vec<u8>) -> ReconnectOutput {
    ReconnectOutput {
        reply: Some(datagram),
        accepted: None,
    }
}

fn rejection(attempt: u32, reason: ReconnectRejection) -> Result<ReconnectOutput, NetError> {
    Ok(reply_with(
        ReconnectMessage::Rejected { attempt, reason }.to_datagram()?,
    ))
}

pub fn reconnect_as_client(
    transport: &Transport,
    host: SocketAddr,
    record: &PeerRecord,
    timing: ReconnectTiming,
) -> Result<SessionKeys, NetError> {
    let mut initiator = ReconnectInitiator::new(record)?;
    let deadline = Instant::now() + timing.timeout;
    let mut buf = [0; RECEIVE_BUFFER_LEN];

    while Instant::now() < deadline {
        if let Some(outgoing) = initiator.outgoing() {
            transport.send_to(outgoing, host)?;
        }

        let resend_at = (Instant::now() + timing.retransmit).min(deadline);
        loop {
            let wait = resend_at.saturating_duration_since(Instant::now());
            let Some((len, from)) = transport.recv_from_timeout(&mut buf, wait)? else {
                break;
            };

            if from != host {
                continue;
            }

            match initiator.handle(&buf[..len]) {
                Ok(ReconnectStep::Complete(keys)) => return Ok(keys),
                Ok(ReconnectStep::Pending) | Err(NetError::Protocol(_)) => {}
                Err(error) => return Err(error),
            }
        }
    }

    Err(NetError::ReconnectTimeout)
}

pub fn reconnect_as_host(
    transport: &Transport,
    responder: &mut ReconnectResponder,
    store: &RwLock<PeerStore>,
    timing: ReconnectTiming,
) -> Result<(SocketAddr, ReconnectAccepted), NetError> {
    let deadline = Instant::now() + timing.timeout;
    let linger = timing.retransmit * LINGER_RETRANSMITS;
    let mut buf = [0; RECEIVE_BUFFER_LEN];
    let mut accepted: Option<(SocketAddr, ReconnectAccepted)> = None;
    let mut linger_until: Option<Instant> = None;

    loop {
        let now = Instant::now();
        let stop_at = linger_until.unwrap_or(deadline);
        if now >= stop_at {
            return accepted.ok_or(NetError::ReconnectTimeout);
        }

        let Some((len, from)) = transport.recv_from_timeout(&mut buf, stop_at - now)? else {
            continue;
        };

        let output = match responder.handle(&buf[..len], from, &store.read()) {
            Ok(output) => output,
            Err(NetError::Protocol(_)) => continue,
            Err(error) => return Err(error),
        };

        if let Some(reply) = output.reply {
            transport.send_to(&reply, from)?;
        }

        if let Some(peer) = output.accepted {
            accepted = Some((from, peer));
            linger_until = Some(Instant::now() + linger);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        thread,
    };

    use rusplay_protocol::{ClientName, StreamKind};

    use super::*;
    use crate::{
        FecScheme, Packetizer, PairingResponder, PairingTiming, Pin, SessionCipher,
        TransportOptions, pair_as_client, pair_as_host, peers::PEER_SECRET_LEN,
    };

    const ORIGIN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 4000);
    const INTRUDER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9)), 4000);
    const LOOPBACK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    const COOKIE_PERIOD: Duration = Duration::from_secs(5);

    fn secret(byte: u8) -> PeerSecret {
        PeerSecret::from_bytes([byte; PEER_SECRET_LEN])
    }

    fn record(name: &str, byte: u8) -> PeerRecord {
        PeerRecord::new(name, secret(byte)).unwrap()
    }

    fn store_with(directory: &tempfile::TempDir, name: &str, records: &[PeerRecord]) -> PeerStore {
        let mut store = PeerStore::open(directory.path().join(name)).unwrap();
        for record in records {
            store.remember(record.clone()).unwrap();
        }
        store
    }

    fn responder() -> ReconnectResponder {
        ReconnectResponder::new(ReconnectTiming {
            retransmit: Duration::from_millis(20),
            timeout: COOKIE_PERIOD,
        })
        .unwrap()
    }

    fn start(initiator: &ReconnectInitiator) -> Vec<u8> {
        initiator.outgoing().unwrap().to_vec()
    }

    fn handle_at(
        responder: &mut ReconnectResponder,
        datagram: &[u8],
        from: SocketAddr,
        store: &PeerStore,
        now: Instant,
    ) -> ReconnectOutput {
        let message = ReconnectMessage::from_datagram(datagram).unwrap();
        responder
            .respond(&message, from, store.iter(), now)
            .unwrap()
    }

    fn challenge_for(
        initiator: &ReconnectInitiator,
        responder: &mut ReconnectResponder,
        store: &PeerStore,
    ) -> Vec<u8> {
        responder
            .handle(&start(initiator), ORIGIN, store)
            .unwrap()
            .reply
            .unwrap()
    }

    fn challenged(
        initiator: &mut ReconnectInitiator,
        responder: &mut ReconnectResponder,
        store: &PeerStore,
    ) -> Vec<u8> {
        let challenge = challenge_for(initiator, responder, store);
        assert!(matches!(
            initiator.handle(&challenge),
            Ok(ReconnectStep::Pending)
        ));
        start(initiator)
    }

    fn handshake(
        initiator: &mut ReconnectInitiator,
        responder: &mut ReconnectResponder,
        store: &PeerStore,
    ) -> (SessionKeys, ReconnectAccepted) {
        let proof = challenged(initiator, responder, store);

        let output = responder.handle(&proof, ORIGIN, store).unwrap();
        let accepted = output.accepted.unwrap();
        let Ok(ReconnectStep::Complete(client)) = initiator.handle(&output.reply.unwrap()) else {
            panic!("client did not complete");
        };

        (client, accepted)
    }

    fn seal_probe(keys: &SessionKeys) -> Vec<u8> {
        let datagram = ReconnectMessage::Accepted { attempt: 1 }
            .to_datagram()
            .unwrap();
        SessionCipher::new(keys).seal(&datagram).unwrap()
    }

    fn assert_keys_interoperate(client: &SessionKeys, host: &SessionKeys) {
        let client = SessionCipher::new(client);
        let host = SessionCipher::new(host);
        let datagram = ReconnectMessage::Accepted { attempt: 1 }
            .to_datagram()
            .unwrap();

        assert_eq!(
            host.open(&client.seal(&datagram).unwrap()).unwrap(),
            datagram
        );
        assert_eq!(
            client.open(&host.seal(&datagram).unwrap()).unwrap(),
            datagram
        );
    }

    fn rejection_of(datagram: &[u8]) -> ReconnectRejection {
        let Ok(ReconnectMessage::Rejected { reason, .. }) =
            ReconnectMessage::from_datagram(datagram)
        else {
            panic!("expected a rejection");
        };
        reason
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn known_peer_reconnects_with_working_session_keys() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();

        let (client, accepted) = handshake(&mut initiator, &mut responder, &store);

        assert_eq!(accepted.peer, laptop.id());
        assert_keys_interoperate(&client, &accepted.keys);
        assert!(initiator.outgoing().is_none());
    }

    #[test]
    fn the_right_peer_is_found_among_many() {
        let directory = tempfile::tempdir().unwrap();
        let records: Vec<PeerRecord> = (1..=20)
            .map(|byte| record(&format!("peer {byte}"), byte))
            .collect();
        let store = store_with(&directory, "clients", &records);
        let mut initiator = ReconnectInitiator::new(&records[13]).unwrap();
        let mut responder = responder();

        let (_, accepted) = handshake(&mut initiator, &mut responder, &store);

        assert_eq!(accepted.peer, records[13].id());
    }

    #[test]
    fn every_reconnect_derives_fresh_keys() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));

        let sealed = |_: u8| {
            let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
            let (client, _) = handshake(&mut initiator, &mut responder(), &store);
            seal_probe(&client)
        };

        assert_ne!(sealed(0), sealed(1));
    }

    #[test]
    fn both_nonces_feed_the_session_keys() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let initiator = ReconnectInitiator::new(&laptop).unwrap();

        let first = challenge_for(&initiator, &mut responder(), &store);
        let second = challenge_for(&initiator, &mut responder(), &store);

        assert_ne!(first, second);
    }

    #[test]
    fn an_observer_cannot_link_two_hellos_of_the_same_peer() {
        let laptop = record("laptop", 1);

        let first = start(&ReconnectInitiator::new(&laptop).unwrap());
        let second = start(&ReconnectInitiator::new(&laptop).unwrap());
        let (
            Ok(ReconnectMessage::Hello {
                nonce: first_nonce,
                blind: first_blind,
                ..
            }),
            Ok(ReconnectMessage::Hello {
                nonce: second_nonce,
                blind: second_blind,
                ..
            }),
        ) = (
            ReconnectMessage::from_datagram(&first),
            ReconnectMessage::from_datagram(&second),
        )
        else {
            panic!("expected two hellos");
        };

        assert_ne!(first_blind, second_blind);
        assert_ne!(first_nonce, second_nonce);
        assert!(!contains(&first, &laptop.id().0));
        assert!(!contains(&second, &laptop.id().0));
    }

    #[test]
    fn unknown_peer_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_with(&directory, "clients", &[record("laptop", 1)]);
        let stranger = record("stranger", 9);
        let mut initiator = ReconnectInitiator::new(&stranger).unwrap();
        let mut responder = responder();

        let reply = challenge_for(&initiator, &mut responder, &store);

        assert_eq!(rejection_of(&reply), ReconnectRejection::UnknownPeer);
        assert!(responder.acceptances.is_empty());
        assert!(matches!(
            initiator.handle(&reply),
            Err(NetError::ReconnectRejected {
                reason: ReconnectRejection::UnknownPeer
            })
        ));
    }

    #[test]
    fn a_hello_blinded_with_a_wrong_secret_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_with(&directory, "clients", &[record("laptop", 1)]);
        let impostor = ReconnectInitiator::new(&record("laptop", 2)).unwrap();

        let reply = challenge_for(&impostor, &mut responder(), &store);

        assert_eq!(rejection_of(&reply), ReconnectRejection::UnknownPeer);
    }

    #[test]
    fn peer_with_a_wrong_secret_never_authenticates() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut impostor = ReconnectInitiator::new(&laptop).unwrap();
        impostor.secret = secret(2);
        let mut responder = responder();

        let challenge = challenge_for(&impostor, &mut responder, &store);

        assert!(matches!(
            impostor.handle(&challenge),
            Err(NetError::ReconnectAuthentication)
        ));
        assert!(impostor.outgoing().is_none());
    }

    #[test]
    fn impostor_host_is_refused_by_the_client() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let fake_store = store_with(&directory, "fake", &[record("laptop", 1)]);
        let mut real = ReconnectInitiator::new(&laptop).unwrap();

        let mut challenge = challenge_for(&real, &mut responder(), &fake_store);
        let last = challenge.len() - 1;
        challenge[last] ^= 1;

        assert!(matches!(
            real.handle(&challenge),
            Err(NetError::ReconnectAuthentication)
        ));
    }

    #[test]
    fn tampered_client_proof_is_rejected_without_keys() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();

        let mut proof = challenged(&mut initiator, &mut responder, &store);
        let last = proof.len() - 1;
        proof[last] ^= 1;
        let output = responder.handle(&proof, ORIGIN, &store).unwrap();

        assert_eq!(
            rejection_of(&output.reply.unwrap()),
            ReconnectRejection::Authentication
        );
        assert!(output.accepted.is_none());
    }

    #[test]
    fn a_forged_proof_does_not_spoil_the_genuine_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();
        let genuine = challenged(&mut initiator, &mut responder, &store);

        let mut forged = genuine.clone();
        let last = forged.len() - 1;
        forged[last] ^= 0xFF;
        responder.handle(&forged, ORIGIN, &store).unwrap();
        let output = responder.handle(&genuine, ORIGIN, &store).unwrap();

        assert!(output.accepted.is_some());
    }

    #[test]
    fn a_proof_with_a_forged_cookie_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();

        let mut proof = challenged(&mut initiator, &mut responder, &store);
        let cookie_at = proof.len() - ReconnectMessage::PROOF_LEN - 1;
        proof[cookie_at] ^= 1;
        let output = responder.handle(&proof, ORIGIN, &store).unwrap();

        assert!(output.reply.is_none());
        assert!(output.accepted.is_none());
    }

    #[test]
    fn a_hello_leaves_no_state_on_the_host() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();

        challenge_for(&initiator, &mut responder, &store);

        assert!(responder.acceptances.is_empty());
    }

    #[test]
    fn a_flood_of_spoofed_hellos_does_not_prevent_a_genuine_reconnect() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut responder = responder();
        let mut genuine = ReconnectInitiator::new(&laptop).unwrap();
        let proof = challenged(&mut genuine, &mut responder, &store);
        let captured = start(&ReconnectInitiator::new(&laptop).unwrap());

        for index in 0..4096_u32 {
            let [_, _, high, low] = index.to_be_bytes();
            let spoofed = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 1, high, low)), 4000);
            let stranger = ReconnectInitiator::new(&record("stranger", 7)).unwrap();
            responder
                .handle(&start(&stranger), spoofed, &store)
                .unwrap();
            responder.handle(&captured, spoofed, &store).unwrap();
        }
        let output = responder.handle(&proof, ORIGIN, &store).unwrap();

        assert!(output.accepted.is_some());
        assert_eq!(responder.acceptances.len(), 1);
    }

    #[test]
    fn a_proof_from_another_address_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();

        let proof = challenged(&mut initiator, &mut responder, &store);
        let replayed = responder.handle(&proof, INTRUDER, &store).unwrap();
        let genuine = responder.handle(&proof, ORIGIN, &store).unwrap();

        assert!(replayed.reply.is_none());
        assert!(replayed.accepted.is_none());
        assert!(genuine.accepted.is_some());
    }

    #[test]
    fn a_hello_echoed_from_another_address_gets_a_challenge_bound_to_that_address() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();

        let genuine = challenge_for(&initiator, &mut responder, &store);
        let echoed = responder
            .handle(&start(&initiator), INTRUDER, &store)
            .unwrap()
            .reply
            .unwrap();

        assert_ne!(genuine, echoed);
        assert!(echoed.len() <= start(&initiator).len());
    }

    #[test]
    fn a_cookie_outlives_one_period_but_expires_after_two() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut responder = responder();
        let issued = Instant::now();

        let mut late = ReconnectInitiator::new(&laptop).unwrap();
        let reply = handle_at(&mut responder, &start(&late), ORIGIN, &store, issued);
        late.handle(&reply.reply.unwrap()).unwrap();
        let mut expired = ReconnectInitiator::new(&laptop).unwrap();
        let reply = handle_at(&mut responder, &start(&expired), ORIGIN, &store, issued);
        expired.handle(&reply.reply.unwrap()).unwrap();

        let in_time = handle_at(
            &mut responder,
            &start(&late),
            ORIGIN,
            &store,
            issued + COOKIE_PERIOD,
        );
        let too_late = handle_at(
            &mut responder,
            &start(&expired),
            ORIGIN,
            &store,
            issued + 2 * COOKIE_PERIOD + Duration::from_millis(1),
        );

        assert!(in_time.accepted.is_some());
        assert!(too_late.reply.is_none());
        assert!(too_late.accepted.is_none());
    }

    #[test]
    fn a_replayed_proof_after_its_cookie_expired_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();
        let issued = Instant::now();
        let reply = handle_at(&mut responder, &start(&initiator), ORIGIN, &store, issued);
        initiator.handle(&reply.reply.unwrap()).unwrap();
        let proof = start(&initiator);

        let first = handle_at(&mut responder, &proof, ORIGIN, &store, issued);
        let replayed = handle_at(
            &mut responder,
            &proof,
            ORIGIN,
            &store,
            issued + 3 * COOKIE_PERIOD,
        );

        assert!(first.accepted.is_some());
        assert!(replayed.reply.is_none());
        assert!(replayed.accepted.is_none());
        assert!(responder.acceptances.is_empty());
    }

    #[test]
    fn forgotten_peer_is_rejected_at_hello() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let mut store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        store.forget(&laptop.id()).unwrap();
        let initiator = ReconnectInitiator::new(&laptop).unwrap();

        let reply = challenge_for(&initiator, &mut responder(), &store);

        assert_eq!(rejection_of(&reply), ReconnectRejection::UnknownPeer);
    }

    #[test]
    fn peer_forgotten_mid_handshake_is_rejected_at_proof() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let mut store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();
        let proof = challenged(&mut initiator, &mut responder, &store);

        store.forget(&laptop.id()).unwrap();
        let output = responder.handle(&proof, ORIGIN, &store).unwrap();

        assert_eq!(
            rejection_of(&output.reply.unwrap()),
            ReconnectRejection::UnknownPeer
        );
        assert!(output.accepted.is_none());
        assert!(responder.acceptances.is_empty());
    }

    #[test]
    fn retransmitted_hello_gets_the_same_challenge() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();

        let first = challenge_for(&initiator, &mut responder, &store);
        let again = challenge_for(&initiator, &mut responder, &store);

        assert_eq!(first, again);
    }

    #[test]
    fn duplicate_proof_repeats_accepted_without_new_keys() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = responder();
        let proof = challenged(&mut initiator, &mut responder, &store);

        let first = responder.handle(&proof, ORIGIN, &store).unwrap();
        let second = responder.handle(&proof, ORIGIN, &store).unwrap();

        assert!(first.accepted.is_some());
        assert!(second.accepted.is_none());
        assert_eq!(first.reply, second.reply);
    }

    #[test]
    fn replayed_proof_of_an_old_attempt_cannot_open_a_new_one() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut old = ReconnectInitiator::new(&laptop).unwrap();
        let captured = challenged(&mut old, &mut responder(), &store);

        let attacker = ReconnectInitiator::new(&laptop).unwrap();
        let mut fresh_responder = responder();
        challenge_for(&attacker, &mut fresh_responder, &store);
        let output = fresh_responder.handle(&captured, ORIGIN, &store).unwrap();

        assert!(output.reply.is_none());
        assert!(output.accepted.is_none());
    }

    #[test]
    fn remembered_acceptances_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut responder = responder();

        for _ in 0..MAX_REMEMBERED_ACCEPTANCES + 8 {
            let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
            handshake(&mut initiator, &mut responder, &store);
        }

        assert_eq!(responder.acceptances.len(), MAX_REMEMBERED_ACCEPTANCES);
    }

    #[test]
    fn client_ignores_messages_of_other_attempts() {
        let mut initiator = ReconnectInitiator::new(&record("laptop", 1)).unwrap();
        let other = ReconnectMessage::Accepted {
            attempt: initiator.attempt().wrapping_add(1),
        }
        .to_datagram()
        .unwrap();

        assert!(matches!(
            initiator.handle(&other),
            Ok(ReconnectStep::Pending)
        ));
        assert!(initiator.outgoing().is_some());
    }

    #[test]
    fn accepted_before_the_challenge_is_ignored() {
        let mut initiator = ReconnectInitiator::new(&record("laptop", 1)).unwrap();
        let early = ReconnectMessage::Accepted {
            attempt: initiator.attempt(),
        }
        .to_datagram()
        .unwrap();

        assert!(matches!(
            initiator.handle(&early),
            Ok(ReconnectStep::Pending)
        ));
    }

    #[test]
    fn pairing_datagrams_are_not_reconnect_traffic() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_with(&directory, "clients", &[]);
        let pairing = rusplay_protocol::PairingMessage::Done { attempt: 1 }
            .to_datagram()
            .unwrap();

        assert!(matches!(
            responder().handle(&pairing, ORIGIN, &store),
            Err(NetError::Protocol(_))
        ));
    }

    fn timing() -> ReconnectTiming {
        ReconnectTiming {
            retransmit: Duration::from_millis(20),
            timeout: Duration::from_secs(5),
        }
    }

    fn transport() -> Transport {
        Transport::bind(LOOPBACK, TransportOptions::default()).unwrap()
    }

    fn media_from(transport: &Transport, peer: &SocketAddr, payload: &[u8]) -> Vec<u8> {
        Packetizer::new(
            StreamKind::Video,
            transport.max_datagram_len(peer),
            FecScheme::None,
        )
        .unwrap()
        .packetize(1, false, payload)
        .unwrap()
        .remove(0)
    }

    #[test]
    fn pair_persist_reload_and_reconnect_without_a_pin() {
        let directory = tempfile::tempdir().unwrap();
        let host_path = directory.path().join("clients");
        let client_path = directory.path().join("hosts");
        let pairing_timing = PairingTiming {
            retransmit: Duration::from_millis(20),
            timeout: Duration::from_secs(5),
        };

        let host_transport = transport();
        let client_transport = transport();
        let host_addr = host_transport.local_addr().unwrap();
        let pin = Pin::parse("424242").unwrap();

        let host_pin = pin.clone();
        let paired_host = thread::spawn(move || {
            let mut responder = PairingResponder::new(host_pin, 3);
            pair_as_host(&host_transport, &mut responder, pairing_timing).unwrap();
            host_transport
        });
        let outcome = pair_as_client(
            &client_transport,
            host_addr,
            &pin,
            ClientName::EMPTY,
            pairing_timing,
        )
        .unwrap();
        let host_transport = paired_host.join().unwrap();

        let mut client_store = PeerStore::open(client_path.clone()).unwrap();
        client_store
            .remember(PeerRecord::new("living room", outcome.secret.clone()).unwrap())
            .unwrap();
        let mut host_store = PeerStore::open(host_path.clone()).unwrap();
        host_store
            .remember(PeerRecord::new("laptop", outcome.secret).unwrap())
            .unwrap();
        drop((client_store, host_store));

        let client_store = PeerStore::open(client_path).unwrap();
        let host_store = RwLock::new(PeerStore::open(host_path).unwrap());
        let known = client_store.iter().next().unwrap().clone();

        let host = thread::spawn(move || {
            let mut responder = ReconnectResponder::new(timing()).unwrap();
            let (peer, accepted) =
                reconnect_as_host(&host_transport, &mut responder, &host_store, timing()).unwrap();
            host_transport.install_session(&accepted.keys);
            (host_transport, peer, accepted.peer)
        });
        let keys = reconnect_as_client(&client_transport, host_addr, &known, timing()).unwrap();
        client_transport.install_session(&keys);
        let (host_transport, peer, accepted_peer) = host.join().unwrap();

        assert_eq!(peer, client_transport.local_addr().unwrap());
        assert_eq!(accepted_peer, known.id());

        let media = media_from(&client_transport, &host_addr, b"secret frame");
        client_transport.send_to(&media, host_addr).unwrap();
        let mut buf = [0; 2048];
        let (len, _) = host_transport.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], media.as_slice());
    }

    #[test]
    fn forgotten_client_cannot_reconnect_over_udp() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let mut host_store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        host_store.forget(&laptop.id()).unwrap();
        let host_store = RwLock::new(host_store);

        let host_transport = transport();
        let client_transport = transport();
        let host_addr = host_transport.local_addr().unwrap();

        let host = thread::spawn(move || {
            let host_timing = ReconnectTiming {
                retransmit: Duration::from_millis(20),
                timeout: Duration::from_millis(400),
            };
            let mut responder = ReconnectResponder::new(host_timing).unwrap();
            reconnect_as_host(&host_transport, &mut responder, &host_store, host_timing)
                .map(|(peer, _)| peer)
        });
        let client = reconnect_as_client(&client_transport, host_addr, &laptop, timing());

        assert!(matches!(
            client,
            Err(NetError::ReconnectRejected {
                reason: ReconnectRejection::UnknownPeer
            })
        ));
        assert!(matches!(
            host.join().unwrap(),
            Err(NetError::ReconnectTimeout)
        ));
    }

    #[test]
    fn client_times_out_when_nobody_answers() {
        let silent = std::net::UdpSocket::bind(LOOPBACK).unwrap();
        let client_transport = transport();

        let result = reconnect_as_client(
            &client_transport,
            silent.local_addr().unwrap(),
            &record("laptop", 1),
            ReconnectTiming {
                retransmit: Duration::from_millis(20),
                timeout: Duration::from_millis(120),
            },
        );

        assert!(matches!(result, Err(NetError::ReconnectTimeout)));
    }

    #[test]
    fn timing_follows_the_network_config() {
        let config = NetworkConfig {
            control_retransmit_ms: 30,
            reconnect_timeout_ms: 7000,
            ..NetworkConfig::default()
        };

        assert_eq!(
            ReconnectTiming::from(&config),
            ReconnectTiming {
                retransmit: Duration::from_millis(30),
                timeout: Duration::from_millis(7000),
            }
        );
    }
}
