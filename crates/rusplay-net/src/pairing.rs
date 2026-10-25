use std::{
    fmt, io,
    net::SocketAddr,
    time::{Duration, Instant},
};

use hkdf::Hkdf;
use rusplay_config::NetworkConfig;
use rusplay_protocol::{PairingMessage, PairingRejection};
use rustix::rand::{GetRandomFlags, getrandom};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::{
    NetError, Transport,
    session::{KEY_LEN, SessionKeys},
};

pub const MIN_PIN_DIGITS: u8 = 4;
pub const MAX_PIN_DIGITS: u8 = 12;

const CLIENT_IDENTITY: &[u8] = b"rusplay client";
const HOST_IDENTITY: &[u8] = b"rusplay host";
const LINGER_RETRANSMITS: u32 = 3;
const RECEIVE_BUFFER_LEN: usize = 2048;

type SpakeBytes = [u8; PairingMessage::SPAKE_LEN];
type Confirmation = [u8; PairingMessage::MAC_LEN];

#[derive(Clone, PartialEq, Eq)]
pub struct Pin(String);

impl Pin {
    pub fn parse(text: &str) -> Result<Self, NetError> {
        let digits = text.trim();
        let valid_length =
            (usize::from(MIN_PIN_DIGITS)..=usize::from(MAX_PIN_DIGITS)).contains(&digits.len());

        if !valid_length || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(NetError::InvalidPin {
                len: digits.chars().count(),
                min: MIN_PIN_DIGITS,
                max: MAX_PIN_DIGITS,
            });
        }

        Ok(Self(digits.to_owned()))
    }

    pub fn generate(digits: u8) -> Result<Self, NetError> {
        if !(MIN_PIN_DIGITS..=MAX_PIN_DIGITS).contains(&digits) {
            return Err(NetError::InvalidPin {
                len: usize::from(digits),
                min: MIN_PIN_DIGITS,
                max: MAX_PIN_DIGITS,
            });
        }

        let modulus = 10u64.pow(u32::from(digits));
        let unbiased_limit = u64::MAX - u64::MAX % modulus;

        let value = loop {
            let mut bytes = [0; 8];
            fill_random(&mut bytes)?;
            let candidate = u64::from_be_bytes(bytes);

            if candidate < unbiased_limit {
                break candidate % modulus;
            }
        };

        Ok(Self(format!(
            "{value:0width$}",
            width = usize::from(digits)
        )))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Pin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Pin").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingTiming {
    pub retransmit: Duration,
    pub timeout: Duration,
}

impl Default for PairingTiming {
    fn default() -> Self {
        Self::from(&NetworkConfig::default())
    }
}

impl From<&NetworkConfig> for PairingTiming {
    fn from(config: &NetworkConfig) -> Self {
        Self {
            retransmit: Duration::from_millis(u64::from(config.control_retransmit_ms)),
            timeout: Duration::from_millis(u64::from(config.pairing_timeout_ms)),
        }
    }
}

fn fill_random(buf: &mut [u8]) -> Result<(), NetError> {
    let mut filled = 0;

    while filled < buf.len() {
        filled += getrandom(&mut buf[filled..], GetRandomFlags::empty())
            .map_err(|errno| NetError::RandomUnavailable(io::Error::from(errno)))?;
    }

    Ok(())
}

struct KeySchedule {
    host_confirm: [u8; KEY_LEN],
    client_confirm: [u8; KEY_LEN],
    client_to_host: [u8; KEY_LEN],
    host_to_client: [u8; KEY_LEN],
}

impl Drop for KeySchedule {
    fn drop(&mut self) {
        self.host_confirm.zeroize();
        self.client_confirm.zeroize();
        self.client_to_host.zeroize();
        self.host_to_client.zeroize();
    }
}

impl KeySchedule {
    fn derive(shared_key: &[u8], attempt: u32) -> Result<Self, NetError> {
        let hkdf = Hkdf::<Sha256>::new(Some(&attempt.to_be_bytes()), shared_key);
        let expand = |label: &[u8]| -> Result<[u8; KEY_LEN], NetError> {
            let mut out = [0; KEY_LEN];
            hkdf.expand(label, &mut out)
                .map_err(NetError::PairingKeyDerivation)?;
            Ok(out)
        };

        Ok(Self {
            host_confirm: expand(b"rusplay pairing host confirm")?,
            client_confirm: expand(b"rusplay pairing client confirm")?,
            client_to_host: expand(b"rusplay session client to host")?,
            host_to_client: expand(b"rusplay session host to client")?,
        })
    }

    fn client_keys(&self) -> SessionKeys {
        SessionKeys::new(self.client_to_host, self.host_to_client)
    }

    fn host_keys(&self) -> SessionKeys {
        SessionKeys::new(self.host_to_client, self.client_to_host)
    }
}

fn confirmation(
    key: &[u8; KEY_LEN],
    label: &[u8],
    attempt: u32,
    client_spake: &SpakeBytes,
    host_spake: &SpakeBytes,
) -> Result<Confirmation, NetError> {
    let mut transcript = Vec::with_capacity(label.len() + 4 + 2 * PairingMessage::SPAKE_LEN);
    transcript.extend_from_slice(label);
    transcript.extend_from_slice(&attempt.to_be_bytes());
    transcript.extend_from_slice(client_spake);
    transcript.extend_from_slice(host_spake);

    let mut tag = [0; PairingMessage::MAC_LEN];
    Hkdf::<Sha256>::new(Some(key), &transcript)
        .expand(b"rusplay pairing confirmation", &mut tag)
        .map_err(NetError::PairingKeyDerivation)?;

    Ok(tag)
}

fn spake_bytes(message: Vec<u8>) -> Result<SpakeBytes, NetError> {
    SpakeBytes::try_from(message)
        .map_err(|_| NetError::PairingKeyExchange(spake2::Error::WrongLength))
}

fn is_equal(a: &Confirmation, b: &Confirmation) -> bool {
    a.ct_eq(b).into()
}

#[derive(Debug)]
pub enum PairingStep {
    Pending,
    Complete(SessionKeys),
}

enum InitiatorStage {
    AwaitReply(Spake2<Ed25519Group>),
    AwaitDone { confirm: Vec<u8>, keys: SessionKeys },
    Finished,
}

pub struct PairingInitiator {
    attempt: u32,
    client_spake: SpakeBytes,
    start: Vec<u8>,
    stage: InitiatorStage,
}

impl fmt::Debug for PairingInitiator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingInitiator")
            .field("attempt", &self.attempt)
            .finish_non_exhaustive()
    }
}

impl PairingInitiator {
    pub fn new(pin: &Pin) -> Result<Self, NetError> {
        let mut attempt_bytes = [0; 4];
        fill_random(&mut attempt_bytes)?;
        let attempt = u32::from_be_bytes(attempt_bytes);

        let (state, message) = Spake2::<Ed25519Group>::start_a(
            &Password::new(pin.as_str()),
            &Identity::new(CLIENT_IDENTITY),
            &Identity::new(HOST_IDENTITY),
        );
        let client_spake = spake_bytes(message)?;
        let start = PairingMessage::Start {
            attempt,
            spake: client_spake,
        }
        .to_datagram()?;

        Ok(Self {
            attempt,
            client_spake,
            start,
            stage: InitiatorStage::AwaitReply(state),
        })
    }

    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn outgoing(&self) -> Option<&[u8]> {
        match &self.stage {
            InitiatorStage::AwaitReply(_) => Some(&self.start),
            InitiatorStage::AwaitDone { confirm, .. } => Some(confirm),
            InitiatorStage::Finished => None,
        }
    }

    pub fn handle(&mut self, datagram: &[u8]) -> Result<PairingStep, NetError> {
        let message = PairingMessage::from_datagram(datagram)?;
        if message.attempt() != self.attempt {
            return Ok(PairingStep::Pending);
        }

        if let PairingMessage::Rejected { reason, .. } = message {
            self.stage = InitiatorStage::Finished;
            return Err(NetError::PairingRejected { reason });
        }

        match (
            std::mem::replace(&mut self.stage, InitiatorStage::Finished),
            message,
        ) {
            (InitiatorStage::AwaitReply(state), PairingMessage::Reply { spake, confirm, .. }) => {
                self.accept_reply(state, spake, confirm)
            }
            (InitiatorStage::AwaitDone { keys, .. }, PairingMessage::Done { .. }) => {
                Ok(PairingStep::Complete(keys))
            }
            (stage, _) => {
                self.stage = stage;
                Ok(PairingStep::Pending)
            }
        }
    }

    fn accept_reply(
        &mut self,
        state: Spake2<Ed25519Group>,
        host_spake: SpakeBytes,
        host_confirm: Confirmation,
    ) -> Result<PairingStep, NetError> {
        let shared = state
            .finish(&host_spake)
            .map_err(NetError::PairingKeyExchange)?;
        let schedule = KeySchedule::derive(&shared, self.attempt)?;

        let expected = confirmation(
            &schedule.host_confirm,
            b"host",
            self.attempt,
            &self.client_spake,
            &host_spake,
        )?;
        if !is_equal(&expected, &host_confirm) {
            return Err(NetError::PairingAuthentication);
        }

        let confirm = PairingMessage::Confirm {
            attempt: self.attempt,
            confirm: confirmation(
                &schedule.client_confirm,
                b"client",
                self.attempt,
                &self.client_spake,
                &host_spake,
            )?,
        }
        .to_datagram()?;

        self.stage = InitiatorStage::AwaitDone {
            confirm,
            keys: schedule.client_keys(),
        };
        Ok(PairingStep::Pending)
    }
}

struct ActiveAttempt {
    attempt: u32,
    reply: Vec<u8>,
    expected_confirm: Confirmation,
    keys: Option<SessionKeys>,
}

struct FinishedAttempt {
    attempt: u32,
    done: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct ResponderOutput {
    pub reply: Option<Vec<u8>>,
    pub keys: Option<SessionKeys>,
}

pub struct PairingResponder {
    pin: Pin,
    attempts_left: u8,
    active: Option<ActiveAttempt>,
    finished: Option<FinishedAttempt>,
    refused_after_exhaustion: bool,
}

impl fmt::Debug for PairingResponder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingResponder")
            .field("attempts_left", &self.attempts_left)
            .finish_non_exhaustive()
    }
}

impl PairingResponder {
    pub const fn new(pin: Pin, max_attempts: u8) -> Self {
        Self {
            pin,
            attempts_left: max_attempts,
            active: None,
            finished: None,
            refused_after_exhaustion: false,
        }
    }

    pub const fn attempts_left(&self) -> u8 {
        self.attempts_left
    }

    pub const fn is_locked(&self) -> bool {
        self.attempts_left == 0
            && self.finished.is_none()
            && (self.active.is_none() || self.refused_after_exhaustion)
    }

    pub const fn is_finished(&self) -> bool {
        self.finished.is_some()
    }

    pub fn handle(&mut self, datagram: &[u8]) -> Result<ResponderOutput, NetError> {
        match PairingMessage::from_datagram(datagram)? {
            PairingMessage::Start { attempt, spake } => self.on_start(attempt, spake),
            PairingMessage::Confirm { attempt, confirm } => self.on_confirm(attempt, &confirm),
            _ => Ok(ResponderOutput::default()),
        }
    }

    fn on_start(
        &mut self,
        attempt: u32,
        client_spake: SpakeBytes,
    ) -> Result<ResponderOutput, NetError> {
        if let Some(finished) = &self.finished {
            return if finished.attempt == attempt {
                Ok(reply_with(finished.done.clone()))
            } else {
                rejection(attempt, PairingRejection::Locked)
            };
        }

        if let Some(active) = &self.active
            && active.attempt == attempt
        {
            return Ok(reply_with(active.reply.clone()));
        }

        if self.attempts_left == 0 {
            self.refused_after_exhaustion = true;
            return rejection(attempt, PairingRejection::Locked);
        }

        let (state, message) = Spake2::<Ed25519Group>::start_b(
            &Password::new(self.pin.as_str()),
            &Identity::new(CLIENT_IDENTITY),
            &Identity::new(HOST_IDENTITY),
        );
        let host_spake = spake_bytes(message)?;
        let shared = state
            .finish(&client_spake)
            .map_err(NetError::PairingKeyExchange)?;
        let schedule = KeySchedule::derive(&shared, attempt)?;

        self.attempts_left -= 1;

        let reply = PairingMessage::Reply {
            attempt,
            spake: host_spake,
            confirm: confirmation(
                &schedule.host_confirm,
                b"host",
                attempt,
                &client_spake,
                &host_spake,
            )?,
        }
        .to_datagram()?;
        self.active = Some(ActiveAttempt {
            attempt,
            reply: reply.clone(),
            expected_confirm: confirmation(
                &schedule.client_confirm,
                b"client",
                attempt,
                &client_spake,
                &host_spake,
            )?,
            keys: Some(schedule.host_keys()),
        });

        Ok(reply_with(reply))
    }

    fn on_confirm(
        &mut self,
        attempt: u32,
        confirm: &Confirmation,
    ) -> Result<ResponderOutput, NetError> {
        if let Some(finished) = &self.finished {
            return Ok(if finished.attempt == attempt {
                reply_with(finished.done.clone())
            } else {
                ResponderOutput::default()
            });
        }

        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.attempt == attempt)
        else {
            return Ok(ResponderOutput::default());
        };

        if !is_equal(&active.expected_confirm, confirm) {
            self.active = None;
            return rejection(attempt, PairingRejection::WrongPin);
        }

        let keys = active.keys.take();
        let done = PairingMessage::Done { attempt }.to_datagram()?;
        self.active = None;
        self.finished = Some(FinishedAttempt {
            attempt,
            done: done.clone(),
        });

        Ok(ResponderOutput {
            reply: Some(done),
            keys,
        })
    }
}

fn reply_with(datagram: Vec<u8>) -> ResponderOutput {
    ResponderOutput {
        reply: Some(datagram),
        keys: None,
    }
}

fn rejection(attempt: u32, reason: PairingRejection) -> Result<ResponderOutput, NetError> {
    Ok(reply_with(
        PairingMessage::Rejected { attempt, reason }.to_datagram()?,
    ))
}

pub fn pair_as_client(
    transport: &Transport,
    host: SocketAddr,
    pin: &Pin,
    timing: PairingTiming,
) -> Result<SessionKeys, NetError> {
    let mut initiator = PairingInitiator::new(pin)?;
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
                Ok(PairingStep::Complete(keys)) => return Ok(keys),
                Ok(PairingStep::Pending) | Err(NetError::Protocol(_)) => {}
                Err(error) => return Err(error),
            }
        }
    }

    Err(NetError::PairingTimeout)
}

pub fn pair_as_host(
    transport: &Transport,
    responder: &mut PairingResponder,
    timing: PairingTiming,
) -> Result<(SocketAddr, SessionKeys), NetError> {
    let deadline = Instant::now() + timing.timeout;
    let linger = timing.retransmit * LINGER_RETRANSMITS;
    let mut buf = [0; RECEIVE_BUFFER_LEN];
    let mut paired: Option<(SocketAddr, SessionKeys)> = None;
    let mut linger_until: Option<Instant> = None;

    loop {
        let now = Instant::now();
        let stop_at = linger_until.unwrap_or(deadline);
        if now >= stop_at {
            return paired.ok_or(NetError::PairingTimeout);
        }

        let Some((len, from)) = transport.recv_from_timeout(&mut buf, stop_at - now)? else {
            continue;
        };

        let output = match responder.handle(&buf[..len]) {
            Ok(output) => output,
            Err(NetError::Protocol(_) | NetError::PairingKeyExchange(_)) => continue,
            Err(error) => return Err(error),
        };

        if let Some(reply) = output.reply {
            transport.send_to(&reply, from)?;
        }

        if let Some(keys) = output.keys {
            paired = Some((from, keys));
            linger_until = Some(Instant::now() + linger);
        }

        if paired.is_none() && responder.is_locked() {
            return Err(NetError::PairingLocked);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        thread,
    };

    use super::*;
    use crate::{SessionCipher, TransportOptions};

    const LOOPBACK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

    fn pin(text: &str) -> Pin {
        Pin::parse(text).unwrap()
    }

    fn start(initiator: &PairingInitiator) -> Vec<u8> {
        initiator.outgoing().unwrap().to_vec()
    }

    fn handshake(
        initiator: &mut PairingInitiator,
        responder: &mut PairingResponder,
    ) -> (SessionKeys, SessionKeys) {
        let reply = responder.handle(&start(initiator)).unwrap().reply.unwrap();
        assert!(matches!(initiator.handle(&reply), Ok(PairingStep::Pending)));

        let done = responder.handle(&start(initiator)).unwrap();
        let host_keys = done.keys.unwrap();
        let Ok(PairingStep::Complete(client_keys)) = initiator.handle(&done.reply.unwrap()) else {
            panic!("client did not complete");
        };

        (client_keys, host_keys)
    }

    fn assert_keys_interoperate(client: &SessionKeys, host: &SessionKeys) {
        let client = SessionCipher::new(client);
        let host = SessionCipher::new(host);
        let datagram = PairingMessage::Done { attempt: 1 }.to_datagram().unwrap();

        assert_eq!(
            host.open(&client.seal(&datagram).unwrap()).unwrap(),
            datagram
        );
        assert_eq!(
            client.open(&host.seal(&datagram).unwrap()).unwrap(),
            datagram
        );
    }

    #[test]
    fn matching_pins_agree_on_working_session_keys() {
        let mut initiator = PairingInitiator::new(&pin("123456")).unwrap();
        let mut responder = PairingResponder::new(pin("123456"), 3);

        let (client, host) = handshake(&mut initiator, &mut responder);

        assert_keys_interoperate(&client, &host);
        assert!(responder.is_finished());
        assert!(initiator.outgoing().is_none());
    }

    #[test]
    fn every_pairing_yields_different_keys() {
        let keys = |_: u8| {
            let mut initiator = PairingInitiator::new(&pin("123456")).unwrap();
            let mut responder = PairingResponder::new(pin("123456"), 3);
            let (client, _) = handshake(&mut initiator, &mut responder);
            let cipher = SessionCipher::new(&client);
            cipher
                .seal(&PairingMessage::Done { attempt: 0 }.to_datagram().unwrap())
                .unwrap()
        };

        assert_ne!(keys(0), keys(1));
    }

    #[test]
    fn wrong_pin_fails_authentication_on_the_client() {
        let mut initiator = PairingInitiator::new(&pin("111111")).unwrap();
        let mut responder = PairingResponder::new(pin("222222"), 3);
        let reply = responder.handle(&start(&initiator)).unwrap().reply.unwrap();

        assert!(matches!(
            initiator.handle(&reply),
            Err(NetError::PairingAuthentication)
        ));
        assert_eq!(responder.attempts_left(), 2);
    }

    #[test]
    fn wrong_confirmation_is_rejected_and_burns_the_attempt() {
        let mut initiator = PairingInitiator::new(&pin("123456")).unwrap();
        let mut responder = PairingResponder::new(pin("123456"), 1);
        let reply = responder.handle(&start(&initiator)).unwrap().reply.unwrap();
        initiator.handle(&reply).unwrap();

        let forged = PairingMessage::Confirm {
            attempt: initiator.attempt(),
            confirm: [0; PairingMessage::MAC_LEN],
        }
        .to_datagram()
        .unwrap();
        let output = responder.handle(&forged).unwrap();

        let rejected = PairingMessage::from_datagram(&output.reply.unwrap()).unwrap();
        assert_eq!(
            rejected,
            PairingMessage::Rejected {
                attempt: initiator.attempt(),
                reason: PairingRejection::WrongPin,
            }
        );
        assert!(output.keys.is_none());
        assert!(responder.is_locked());
    }

    #[test]
    fn retransmitted_start_does_not_consume_an_attempt() {
        let initiator = PairingInitiator::new(&pin("123456")).unwrap();
        let mut responder = PairingResponder::new(pin("123456"), 3);

        let first = responder.handle(&start(&initiator)).unwrap().reply.unwrap();
        let again = responder.handle(&start(&initiator)).unwrap().reply.unwrap();

        assert_eq!(first, again);
        assert_eq!(responder.attempts_left(), 2);
    }

    #[test]
    fn every_fresh_start_costs_an_attempt_until_locked() {
        let mut responder = PairingResponder::new(pin("123456"), 2);

        for _ in 0..2 {
            let initiator = PairingInitiator::new(&pin("000000")).unwrap();
            let reply = responder.handle(&start(&initiator)).unwrap().reply.unwrap();
            assert!(matches!(
                PairingMessage::from_datagram(&reply),
                Ok(PairingMessage::Reply { .. })
            ));
        }

        let late = PairingInitiator::new(&pin("123456")).unwrap();
        let reply = responder.handle(&start(&late)).unwrap().reply.unwrap();

        assert_eq!(
            PairingMessage::from_datagram(&reply),
            Ok(PairingMessage::Rejected {
                attempt: late.attempt(),
                reason: PairingRejection::Locked,
            })
        );
        assert_eq!(responder.attempts_left(), 0);
    }

    #[test]
    fn finished_responder_refuses_new_attempts_but_repeats_done() {
        let mut initiator = PairingInitiator::new(&pin("123456")).unwrap();
        let mut responder = PairingResponder::new(pin("123456"), 3);
        handshake(&mut initiator, &mut responder);

        let restart = PairingMessage::Start {
            attempt: initiator.attempt(),
            spake: [0; PairingMessage::SPAKE_LEN],
        }
        .to_datagram()
        .unwrap();
        let repeat = responder.handle(&restart).unwrap();
        assert!(matches!(
            PairingMessage::from_datagram(&repeat.reply.unwrap()),
            Ok(PairingMessage::Done { .. })
        ));
        assert!(repeat.keys.is_none());

        let stranger = PairingInitiator::new(&pin("123456")).unwrap();
        let reply = responder.handle(&start(&stranger)).unwrap().reply.unwrap();
        assert!(matches!(
            PairingMessage::from_datagram(&reply),
            Ok(PairingMessage::Rejected {
                reason: PairingRejection::Locked,
                ..
            })
        ));
    }

    #[test]
    fn duplicate_confirm_repeats_done_without_new_keys() {
        let mut initiator = PairingInitiator::new(&pin("123456")).unwrap();
        let mut responder = PairingResponder::new(pin("123456"), 3);
        let reply = responder.handle(&start(&initiator)).unwrap().reply.unwrap();
        initiator.handle(&reply).unwrap();
        let confirm = start(&initiator);

        let first = responder.handle(&confirm).unwrap();
        let second = responder.handle(&confirm).unwrap();

        assert!(first.keys.is_some());
        assert!(second.keys.is_none());
        assert_eq!(first.reply, second.reply);
    }

    #[test]
    fn client_ignores_messages_of_other_attempts() {
        let mut initiator = PairingInitiator::new(&pin("123456")).unwrap();
        let other = PairingMessage::Done {
            attempt: initiator.attempt().wrapping_add(1),
        }
        .to_datagram()
        .unwrap();

        assert!(matches!(initiator.handle(&other), Ok(PairingStep::Pending)));
        assert!(initiator.outgoing().is_some());
    }

    #[test]
    fn client_surfaces_a_rejection() {
        let mut initiator = PairingInitiator::new(&pin("123456")).unwrap();
        let rejection = PairingMessage::Rejected {
            attempt: initiator.attempt(),
            reason: PairingRejection::Locked,
        }
        .to_datagram()
        .unwrap();

        assert!(matches!(
            initiator.handle(&rejection),
            Err(NetError::PairingRejected {
                reason: PairingRejection::Locked
            })
        ));
    }

    #[test]
    fn corrupt_key_exchange_message_is_an_error_and_costs_nothing() {
        let mut responder = PairingResponder::new(pin("123456"), 3);
        let forged = PairingMessage::Start {
            attempt: 1,
            spake: [0xFF; PairingMessage::SPAKE_LEN],
        }
        .to_datagram()
        .unwrap();

        assert!(matches!(
            responder.handle(&forged),
            Err(NetError::PairingKeyExchange(_))
        ));
        assert_eq!(responder.attempts_left(), 3);
    }

    #[test]
    fn tampered_host_confirmation_aborts_the_client() {
        let mut initiator = PairingInitiator::new(&pin("123456")).unwrap();
        let mut responder = PairingResponder::new(pin("123456"), 3);
        let mut reply = responder.handle(&start(&initiator)).unwrap().reply.unwrap();
        let last = reply.len() - 1;
        reply[last] ^= 1;

        assert!(matches!(
            initiator.handle(&reply),
            Err(NetError::PairingAuthentication)
        ));
    }

    #[test]
    fn pins_parse_trim_and_validate() {
        assert_eq!(pin(" 0042 ").as_str(), "0042");
        for bad in ["", "123", "1234567890123", "12a456", "12 456", "١٢٣٤٥٦"] {
            assert!(
                matches!(Pin::parse(bad), Err(NetError::InvalidPin { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn generated_pins_have_the_requested_digits() {
        for digits in MIN_PIN_DIGITS..=MAX_PIN_DIGITS {
            let generated = Pin::generate(digits).unwrap();

            assert_eq!(generated.as_str().len(), usize::from(digits));
            assert!(generated.as_str().bytes().all(|byte| byte.is_ascii_digit()));
        }
        assert!(matches!(
            Pin::generate(MIN_PIN_DIGITS - 1),
            Err(NetError::InvalidPin { .. })
        ));
        assert!(matches!(
            Pin::generate(MAX_PIN_DIGITS + 1),
            Err(NetError::InvalidPin { .. })
        ));
    }

    #[test]
    fn generated_pins_vary() {
        let pins: Vec<String> = (0..8)
            .map(|_| Pin::generate(9).unwrap().as_str().to_owned())
            .collect();

        assert!(pins.iter().any(|pin| pin != &pins[0]));
    }

    #[test]
    fn pin_debug_output_hides_the_digits() {
        assert!(!format!("{:?}", pin("987654")).contains("987654"));
    }

    fn timing() -> PairingTiming {
        PairingTiming {
            retransmit: Duration::from_millis(20),
            timeout: Duration::from_secs(5),
        }
    }

    fn transport() -> Transport {
        Transport::bind(LOOPBACK, TransportOptions::default()).unwrap()
    }

    #[test]
    fn pairing_over_udp_installs_matching_sessions() {
        let host_transport = transport();
        let client_transport = transport();
        let host_addr = host_transport.local_addr().unwrap();

        let host = thread::spawn(move || {
            let mut responder = PairingResponder::new(pin("424242"), 3);
            let (peer, keys) = pair_as_host(&host_transport, &mut responder, timing()).unwrap();
            host_transport.install_session(&keys);
            (host_transport, peer)
        });
        let keys = pair_as_client(&client_transport, host_addr, &pin("424242"), timing()).unwrap();
        client_transport.install_session(&keys);
        let (host_transport, peer) = host.join().unwrap();

        assert_eq!(peer, client_transport.local_addr().unwrap());
        let datagram = PairingMessage::Done { attempt: 9 }.to_datagram().unwrap();
        let media = {
            let mut tx = crate::Packetizer::new(
                rusplay_protocol::StreamKind::Video,
                client_transport.max_datagram_len(&host_addr),
                crate::FecScheme::None,
            )
            .unwrap();
            tx.packetize(1, false, b"secret frame").unwrap().remove(0)
        };
        client_transport.send_to(&media, host_addr).unwrap();
        client_transport.send_to(&datagram, host_addr).unwrap();

        let mut buf = [0; 2048];
        let (len, _) = host_transport.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], media.as_slice());
        let (len, _) = host_transport.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], datagram.as_slice());
    }

    #[test]
    fn sessions_refuse_plaintext_media_and_foreign_ciphertext() {
        let host_transport = transport();
        let intruder = transport();
        let host_addr = host_transport.local_addr().unwrap();
        host_transport.install_session(&SessionKeys::new([1; KEY_LEN], [2; KEY_LEN]));

        let media = crate::Packetizer::new(
            rusplay_protocol::StreamKind::Video,
            1400,
            crate::FecScheme::None,
        )
        .unwrap()
        .packetize(1, false, b"forged")
        .unwrap()
        .remove(0);
        intruder.send_to(&media, host_addr).unwrap();
        let foreign = SessionCipher::new(&SessionKeys::new([7; KEY_LEN], [7; KEY_LEN]));
        intruder
            .socket()
            .send_to(&foreign.seal(&media).unwrap(), host_addr)
            .unwrap();
        host_transport.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let mut buf = [0; 2048];
        assert!(host_transport.try_recv_from(&mut buf).unwrap().is_none());
        assert_eq!(host_transport.rejected_datagrams(), 2);
    }

    #[test]
    fn wrong_pin_over_udp_fails_both_ends() {
        let host_transport = transport();
        let client_transport = transport();
        let host_addr = host_transport.local_addr().unwrap();

        let host = thread::spawn(move || {
            let mut responder = PairingResponder::new(pin("111111"), 3);
            pair_as_host(
                &host_transport,
                &mut responder,
                PairingTiming {
                    retransmit: Duration::from_millis(20),
                    timeout: Duration::from_millis(400),
                },
            )
            .map(|(peer, _)| peer)
        });
        let client = pair_as_client(&client_transport, host_addr, &pin("222222"), timing());

        assert!(matches!(client, Err(NetError::PairingAuthentication)));
        assert!(matches!(
            host.join().unwrap(),
            Err(NetError::PairingTimeout)
        ));
    }

    #[test]
    fn exhausted_attempts_lock_the_host_driver() {
        let host_transport = transport();
        let client_transport = transport();
        let host_addr = host_transport.local_addr().unwrap();

        let host = thread::spawn(move || {
            let mut responder = PairingResponder::new(pin("111111"), 1);
            pair_as_host(&host_transport, &mut responder, timing()).map(|(peer, _)| peer)
        });
        let first = pair_as_client(&client_transport, host_addr, &pin("222222"), timing());
        let second = pair_as_client(
            &client_transport,
            host_addr,
            &pin("111111"),
            PairingTiming {
                retransmit: Duration::from_millis(20),
                timeout: Duration::from_millis(300),
            },
        );

        assert!(matches!(first, Err(NetError::PairingAuthentication)));
        assert!(matches!(
            second,
            Err(NetError::PairingRejected {
                reason: PairingRejection::Locked
            })
        ));
        assert!(matches!(host.join().unwrap(), Err(NetError::PairingLocked)));
    }

    #[test]
    fn client_times_out_when_nobody_answers() {
        let silent = UdpSocketHolder::new();
        let client_transport = transport();

        let result = pair_as_client(
            &client_transport,
            silent.addr,
            &pin("123456"),
            PairingTiming {
                retransmit: Duration::from_millis(20),
                timeout: Duration::from_millis(120),
            },
        );

        assert!(matches!(result, Err(NetError::PairingTimeout)));
    }

    struct UdpSocketHolder {
        _socket: std::net::UdpSocket,
        addr: SocketAddr,
    }

    impl UdpSocketHolder {
        fn new() -> Self {
            let socket = std::net::UdpSocket::bind(LOOPBACK).unwrap();
            let addr = socket.local_addr().unwrap();
            Self {
                _socket: socket,
                addr,
            }
        }
    }

    #[test]
    fn timing_follows_the_network_config() {
        let config = NetworkConfig {
            control_retransmit_ms: 30,
            pairing_timeout_ms: 9000,
            ..NetworkConfig::default()
        };

        assert_eq!(
            PairingTiming::from(&config),
            PairingTiming {
                retransmit: Duration::from_millis(30),
                timeout: Duration::from_millis(9000),
            }
        );
    }
}
