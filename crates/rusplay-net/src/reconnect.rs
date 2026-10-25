use std::{
    collections::VecDeque,
    fmt,
    net::SocketAddr,
    time::{Duration, Instant},
};

use hkdf::Hkdf;
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
const MAX_TRACKED_ATTEMPTS: usize = 16;

type Nonce = [u8; ReconnectMessage::NONCE_LEN];
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

fn proofs_match(a: &Proof, b: &Proof) -> bool {
    a.ct_eq(b).into()
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
        host_nonce: &Nonce,
    ) -> Result<Self, NetError> {
        let mut salt = Vec::with_capacity(PeerId::LEN + 4 + 2 * ReconnectMessage::NONCE_LEN);
        salt.extend_from_slice(&peer.0);
        salt.extend_from_slice(&attempt.to_be_bytes());
        salt.extend_from_slice(client_nonce);
        salt.extend_from_slice(host_nonce);

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
        let peer = record.id();
        let hello = ReconnectMessage::Hello {
            attempt,
            peer,
            nonce: client_nonce,
        }
        .to_datagram()?;

        Ok(Self {
            attempt,
            peer,
            secret: record.secret().clone(),
            client_nonce,
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
            (InitiatorStage::AwaitChallenge, ReconnectMessage::Challenge { nonce, proof, .. }) => {
                self.accept_challenge(&nonce, &proof)
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
        host_nonce: &Nonce,
        host_proof: &Proof,
    ) -> Result<ReconnectStep, NetError> {
        let schedule = Schedule::derive(
            &self.secret,
            &self.peer,
            self.attempt,
            &self.client_nonce,
            host_nonce,
        )?;

        if !proofs_match(&schedule.host_proof, host_proof) {
            return Err(NetError::ReconnectAuthentication);
        }

        let proof = ReconnectMessage::Proof {
            attempt: self.attempt,
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

enum EntryState {
    Awaiting {
        expected_proof: Proof,
        keys: Option<SessionKeys>,
    },
    Accepted,
}

struct Entry {
    attempt: u32,
    peer: PeerId,
    client_nonce: Nonce,
    reply: Vec<u8>,
    state: EntryState,
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

#[derive(Default)]
pub struct ReconnectResponder {
    entries: VecDeque<Entry>,
}

impl fmt::Debug for ReconnectResponder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconnectResponder")
            .field("tracked_attempts", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl ReconnectResponder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tracked_attempts(&self) -> usize {
        self.entries.len()
    }

    pub fn handle(
        &mut self,
        datagram: &[u8],
        store: &PeerStore,
    ) -> Result<ReconnectOutput, NetError> {
        match ReconnectMessage::from_datagram(datagram)? {
            ReconnectMessage::Hello {
                attempt,
                peer,
                nonce,
            } => self.on_hello(attempt, peer, nonce, store),
            ReconnectMessage::Proof { attempt, proof } => self.on_proof(attempt, &proof, store),
            _ => Ok(ReconnectOutput::default()),
        }
    }

    fn on_hello(
        &mut self,
        attempt: u32,
        peer: PeerId,
        client_nonce: Nonce,
        store: &PeerStore,
    ) -> Result<ReconnectOutput, NetError> {
        if let Some(entry) = self.entries.iter().find(|entry| entry.attempt == attempt) {
            return Ok(
                if entry.peer == peer && entry.client_nonce == client_nonce {
                    reply_with(entry.reply.clone())
                } else {
                    ReconnectOutput::default()
                },
            );
        }

        let Some(record) = store.get(&peer) else {
            return rejection(attempt, ReconnectRejection::UnknownPeer);
        };

        let host_nonce = random_nonce()?;
        let schedule =
            Schedule::derive(record.secret(), &peer, attempt, &client_nonce, &host_nonce)?;
        let reply = ReconnectMessage::Challenge {
            attempt,
            nonce: host_nonce,
            proof: schedule.host_proof,
        }
        .to_datagram()?;

        if self.entries.len() >= MAX_TRACKED_ATTEMPTS {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry {
            attempt,
            peer,
            client_nonce,
            reply: reply.clone(),
            state: EntryState::Awaiting {
                expected_proof: schedule.client_proof,
                keys: Some(schedule.host_keys()),
            },
        });

        Ok(reply_with(reply))
    }

    fn on_proof(
        &mut self,
        attempt: u32,
        proof: &Proof,
        store: &PeerStore,
    ) -> Result<ReconnectOutput, NetError> {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.attempt == attempt)
        else {
            return Ok(ReconnectOutput::default());
        };

        let entry = &mut self.entries[index];
        let EntryState::Awaiting {
            expected_proof,
            keys,
        } = &mut entry.state
        else {
            return Ok(reply_with(entry.reply.clone()));
        };

        if store.get(&entry.peer).is_none() {
            self.entries.remove(index);
            return rejection(attempt, ReconnectRejection::UnknownPeer);
        }

        if !proofs_match(expected_proof, proof) {
            return rejection(attempt, ReconnectRejection::Authentication);
        }

        let keys = keys.take();
        let peer = entry.peer;
        let accepted = ReconnectMessage::Accepted { attempt }.to_datagram()?;
        entry.reply.clone_from(&accepted);
        entry.state = EntryState::Accepted;

        Ok(ReconnectOutput {
            reply: Some(accepted),
            accepted: keys.map(|keys| ReconnectAccepted { peer, keys }),
        })
    }
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

        let output = match responder.handle(&buf[..len], &store.read()) {
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

    use rusplay_protocol::StreamKind;

    use super::*;
    use crate::{
        FecScheme, Packetizer, PairingResponder, PairingTiming, Pin, SessionCipher,
        TransportOptions, pair_as_client, pair_as_host, peers::PEER_SECRET_LEN,
    };

    const LOOPBACK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

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

    fn start(initiator: &ReconnectInitiator) -> Vec<u8> {
        initiator.outgoing().unwrap().to_vec()
    }

    fn challenge_for(
        initiator: &ReconnectInitiator,
        responder: &mut ReconnectResponder,
        store: &PeerStore,
    ) -> Vec<u8> {
        responder
            .handle(&start(initiator), store)
            .unwrap()
            .reply
            .unwrap()
    }

    fn handshake(
        initiator: &mut ReconnectInitiator,
        responder: &mut ReconnectResponder,
        store: &PeerStore,
    ) -> (SessionKeys, ReconnectAccepted) {
        let challenge = challenge_for(initiator, responder, store);
        assert!(matches!(
            initiator.handle(&challenge),
            Ok(ReconnectStep::Pending)
        ));

        let output = responder.handle(&start(initiator), store).unwrap();
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

    #[test]
    fn known_peer_reconnects_with_working_session_keys() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = ReconnectResponder::new();

        let (client, accepted) = handshake(&mut initiator, &mut responder, &store);

        assert_eq!(accepted.peer, laptop.id());
        assert_keys_interoperate(&client, &accepted.keys);
        assert!(initiator.outgoing().is_none());
    }

    #[test]
    fn every_reconnect_derives_fresh_keys() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));

        let sealed = |_: u8| {
            let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
            let mut responder = ReconnectResponder::new();
            let (client, _) = handshake(&mut initiator, &mut responder, &store);
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

        let first = challenge_for(&initiator, &mut ReconnectResponder::new(), &store);
        let second = challenge_for(&initiator, &mut ReconnectResponder::new(), &store);

        assert_ne!(first, second);
    }

    #[test]
    fn unknown_peer_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_with(&directory, "clients", &[record("laptop", 1)]);
        let stranger = record("stranger", 9);
        let mut initiator = ReconnectInitiator::new(&stranger).unwrap();
        let mut responder = ReconnectResponder::new();

        let reply = challenge_for(&initiator, &mut responder, &store);

        assert_eq!(rejection_of(&reply), ReconnectRejection::UnknownPeer);
        assert_eq!(responder.tracked_attempts(), 0);
        assert!(matches!(
            initiator.handle(&reply),
            Err(NetError::ReconnectRejected {
                reason: ReconnectRejection::UnknownPeer
            })
        ));
    }

    #[test]
    fn peer_with_a_wrong_secret_never_authenticates() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut impostor = ReconnectInitiator::new(&laptop).unwrap();
        impostor.secret = secret(2);
        let mut responder = ReconnectResponder::new();

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
        let mut fake_host = ReconnectResponder::new();

        let mut challenge = challenge_for(&real, &mut fake_host, &fake_store);
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
        let mut responder = ReconnectResponder::new();
        let challenge = challenge_for(&initiator, &mut responder, &store);
        initiator.handle(&challenge).unwrap();

        let mut proof = start(&initiator);
        let last = proof.len() - 1;
        proof[last] ^= 1;
        let output = responder.handle(&proof, &store).unwrap();

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
        let mut responder = ReconnectResponder::new();
        let challenge = challenge_for(&initiator, &mut responder, &store);
        initiator.handle(&challenge).unwrap();

        let forged = ReconnectMessage::Proof {
            attempt: initiator.attempt(),
            proof: [0; ReconnectMessage::PROOF_LEN],
        }
        .to_datagram()
        .unwrap();
        responder.handle(&forged, &store).unwrap();
        let output = responder.handle(&start(&initiator), &store).unwrap();

        assert!(output.accepted.is_some());
    }

    #[test]
    fn forgotten_peer_is_rejected_at_hello() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let mut store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        store.forget(&laptop.id()).unwrap();
        let initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = ReconnectResponder::new();

        let reply = challenge_for(&initiator, &mut responder, &store);

        assert_eq!(rejection_of(&reply), ReconnectRejection::UnknownPeer);
    }

    #[test]
    fn peer_forgotten_mid_handshake_is_rejected_at_proof() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let mut store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = ReconnectResponder::new();
        let challenge = challenge_for(&initiator, &mut responder, &store);
        initiator.handle(&challenge).unwrap();

        store.forget(&laptop.id()).unwrap();
        let output = responder.handle(&start(&initiator), &store).unwrap();

        assert_eq!(
            rejection_of(&output.reply.unwrap()),
            ReconnectRejection::UnknownPeer
        );
        assert!(output.accepted.is_none());
        assert_eq!(responder.tracked_attempts(), 0);
    }

    #[test]
    fn retransmitted_hello_gets_the_same_challenge() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = ReconnectResponder::new();

        let first = challenge_for(&initiator, &mut responder, &store);
        let again = challenge_for(&initiator, &mut responder, &store);

        assert_eq!(first, again);
        assert_eq!(responder.tracked_attempts(), 1);
    }

    #[test]
    fn duplicate_proof_repeats_accepted_without_new_keys() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut initiator = ReconnectInitiator::new(&laptop).unwrap();
        let mut responder = ReconnectResponder::new();
        let challenge = challenge_for(&initiator, &mut responder, &store);
        initiator.handle(&challenge).unwrap();
        let proof = start(&initiator);

        let first = responder.handle(&proof, &store).unwrap();
        let second = responder.handle(&proof, &store).unwrap();

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
        let mut responder = ReconnectResponder::new();
        let challenge = challenge_for(&old, &mut responder, &store);
        old.handle(&challenge).unwrap();
        let captured = start(&old);

        let attacker = ReconnectInitiator::new(&laptop).unwrap();
        let mut fresh_responder = ReconnectResponder::new();
        challenge_for(&attacker, &mut fresh_responder, &store);
        let output = fresh_responder.handle(&captured, &store).unwrap();

        assert!(output.reply.is_none());
        assert!(output.accepted.is_none());
    }

    #[test]
    fn tracked_attempts_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let laptop = record("laptop", 1);
        let store = store_with(&directory, "clients", std::slice::from_ref(&laptop));
        let mut responder = ReconnectResponder::new();

        for _ in 0..MAX_TRACKED_ATTEMPTS * 2 {
            let initiator = ReconnectInitiator::new(&laptop).unwrap();
            challenge_for(&initiator, &mut responder, &store);
        }

        assert_eq!(responder.tracked_attempts(), MAX_TRACKED_ATTEMPTS);
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
        let mut responder = ReconnectResponder::new();
        let pairing = rusplay_protocol::PairingMessage::Done { attempt: 1 }
            .to_datagram()
            .unwrap();

        assert!(matches!(
            responder.handle(&pairing, &store),
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
        let outcome = pair_as_client(&client_transport, host_addr, &pin, pairing_timing).unwrap();
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
            let mut responder = ReconnectResponder::new();
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
            let mut responder = ReconnectResponder::new();
            reconnect_as_host(
                &host_transport,
                &mut responder,
                &host_store,
                ReconnectTiming {
                    retransmit: Duration::from_millis(20),
                    timeout: Duration::from_millis(400),
                },
            )
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
