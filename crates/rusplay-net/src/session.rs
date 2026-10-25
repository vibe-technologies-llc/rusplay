use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use chacha20poly1305::{AeadInOut, ChaCha20Poly1305, Key, KeyInit, Nonce, Tag};
use parking_lot::Mutex;
use rusplay_protocol::{PacketHeader, StreamKind};
use zeroize::Zeroize;

use crate::NetError;

pub const KEY_LEN: usize = 32;
const COUNTER_LEN: usize = 8;
const TAG_LEN: usize = 16;
const REPLAY_WINDOW: u64 = 64;

pub const SEALED_OVERHEAD: usize = COUNTER_LEN + TAG_LEN;

pub const fn requires_sealing(stream: StreamKind) -> bool {
    !matches!(
        stream,
        StreamKind::Discovery | StreamKind::Pairing | StreamKind::Reconnect
    )
}

pub struct SessionKeys {
    send: [u8; KEY_LEN],
    receive: [u8; KEY_LEN],
}

impl SessionKeys {
    pub const fn new(send: [u8; KEY_LEN], receive: [u8; KEY_LEN]) -> Self {
        Self { send, receive }
    }
}

impl Drop for SessionKeys {
    fn drop(&mut self) {
        self.send.zeroize();
        self.receive.zeroize();
    }
}

impl fmt::Debug for SessionKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionKeys")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct ReplayWindow {
    highest: Option<u64>,
    seen: u64,
}

impl ReplayWindow {
    fn admits(&self, counter: u64) -> bool {
        let Some(highest) = self.highest else {
            return true;
        };

        if counter > highest {
            return true;
        }

        let behind = highest - counter;
        behind < REPLAY_WINDOW && self.seen & (1 << behind) == 0
    }

    fn record(&mut self, counter: u64) {
        match self.highest {
            Some(highest) if counter <= highest => self.seen |= 1 << (highest - counter),
            Some(highest) => {
                let shift = counter - highest;
                self.seen = u32::try_from(shift)
                    .ok()
                    .and_then(|shift| self.seen.checked_shl(shift))
                    .unwrap_or(0)
                    | 1;
                self.highest = Some(counter);
            }
            None => {
                self.highest = Some(counter);
                self.seen = 1;
            }
        }
    }
}

pub struct SessionCipher {
    send: ChaCha20Poly1305,
    receive: ChaCha20Poly1305,
    send_counter: AtomicU64,
    replay: Mutex<ReplayWindow>,
}

impl fmt::Debug for SessionCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionCipher")
            .finish_non_exhaustive()
    }
}

impl SessionCipher {
    pub fn new(keys: &SessionKeys) -> Self {
        Self {
            send: ChaCha20Poly1305::new(&Key::from(keys.send)),
            receive: ChaCha20Poly1305::new(&Key::from(keys.receive)),
            send_counter: AtomicU64::new(0),
            replay: Mutex::new(ReplayWindow::default()),
        }
    }

    pub fn seal(&self, datagram: &[u8]) -> Result<Vec<u8>, NetError> {
        let header =
            datagram
                .get(..PacketHeader::SIZE)
                .ok_or(NetError::SessionDatagramTooShort {
                    len: datagram.len(),
                    min: PacketHeader::SIZE,
                })?;
        let counter = self.send_counter.fetch_add(1, Ordering::Relaxed);

        let mut sealed = Vec::with_capacity(datagram.len() + SEALED_OVERHEAD);
        sealed.extend_from_slice(header);
        sealed.extend_from_slice(&counter.to_be_bytes());
        sealed.extend_from_slice(&datagram[PacketHeader::SIZE..]);

        let tag = self
            .send
            .encrypt_inout_detached(
                &nonce_for(counter),
                header,
                (&mut sealed[PacketHeader::SIZE + COUNTER_LEN..]).into(),
            )
            .map_err(NetError::SessionSeal)?;
        sealed.extend_from_slice(&tag);

        Ok(sealed)
    }

    pub fn open_in_place(&self, sealed: &mut [u8]) -> Result<usize, NetError> {
        let min = PacketHeader::SIZE + SEALED_OVERHEAD;
        if sealed.len() < min {
            return Err(NetError::SessionDatagramTooShort {
                len: sealed.len(),
                min,
            });
        }

        let counter_start = PacketHeader::SIZE;
        let payload_start = counter_start + COUNTER_LEN;
        let tag_start = sealed.len() - TAG_LEN;

        let mut counter_bytes = [0; COUNTER_LEN];
        counter_bytes.copy_from_slice(&sealed[counter_start..payload_start]);
        let counter = u64::from_be_bytes(counter_bytes);
        let tag =
            Tag::try_from(&sealed[tag_start..]).map_err(|_| NetError::SessionDatagramTooShort {
                len: sealed.len(),
                min,
            })?;

        let mut replay = self.replay.lock();
        if !replay.admits(counter) {
            return Err(NetError::SessionReplay { counter });
        }

        let (head, rest) = sealed.split_at_mut(payload_start);
        let ciphertext = &mut rest[..tag_start - payload_start];
        self.receive
            .decrypt_inout_detached(
                &nonce_for(counter),
                &head[..PacketHeader::SIZE],
                ciphertext.into(),
                &tag,
            )
            .map_err(|_| NetError::SessionAuthentication)?;
        replay.record(counter);
        drop(replay);

        let payload_len = tag_start - payload_start;
        sealed.copy_within(
            payload_start..payload_start + payload_len,
            PacketHeader::SIZE,
        );
        Ok(PacketHeader::SIZE + payload_len)
    }

    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, NetError> {
        let mut buffer = sealed.to_vec();
        let len = self.open_in_place(&mut buffer)?;
        buffer.truncate(len);

        Ok(buffer)
    }
}

fn nonce_for(counter: u64) -> Nonce {
    let mut bytes = [0; 12];
    bytes[4..].copy_from_slice(&counter.to_be_bytes());

    Nonce::from(bytes)
}

#[cfg(test)]
mod tests {
    use rusplay_protocol::{PacketFlags, ProtocolVersion};

    use super::*;

    fn keys(a: u8, b: u8) -> SessionKeys {
        SessionKeys::new([a; KEY_LEN], [b; KEY_LEN])
    }

    fn pair() -> (SessionCipher, SessionCipher) {
        (
            SessionCipher::new(&keys(1, 2)),
            SessionCipher::new(&keys(2, 1)),
        )
    }

    fn datagram(payload: &[u8]) -> Vec<u8> {
        let header = PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Video,
            flags: PacketFlags::KEYFRAME,
            sequence: 5,
            frame: 6,
            fragment_index: 0,
            fragment_count: 1,
        };
        let mut datagram = vec![0; PacketHeader::SIZE];
        header.write(&mut datagram).unwrap();
        datagram.extend_from_slice(payload);
        datagram
    }

    #[test]
    fn sealed_datagram_round_trips() {
        let (client, host) = pair();
        let plain = datagram(b"frame bytes");

        let sealed = client.seal(&plain).unwrap();

        assert_eq!(sealed.len(), plain.len() + SEALED_OVERHEAD);
        assert_eq!(&sealed[..PacketHeader::SIZE], &plain[..PacketHeader::SIZE]);
        assert_ne!(
            &sealed[PacketHeader::SIZE + COUNTER_LEN..sealed.len() - TAG_LEN],
            b"frame bytes"
        );
        assert_eq!(host.open(&sealed).unwrap(), plain);
    }

    #[test]
    fn empty_payload_round_trips() {
        let (client, host) = pair();
        let plain = datagram(&[]);

        assert_eq!(host.open(&client.seal(&plain).unwrap()).unwrap(), plain);
    }

    #[test]
    fn directions_use_different_keys() {
        let (client, _) = pair();
        let sealed = client.seal(&datagram(b"x")).unwrap();

        assert!(matches!(
            client.open(&sealed),
            Err(NetError::SessionAuthentication)
        ));
    }

    #[test]
    fn every_nonce_is_fresh() {
        let (client, _) = pair();
        let plain = datagram(b"same");

        let first = client.seal(&plain).unwrap();
        let second = client.seal(&plain).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn tampering_anywhere_is_detected() {
        let (client, host) = pair();
        let sealed = client.seal(&datagram(b"important payload")).unwrap();

        for index in 0..sealed.len() {
            let mut forged = sealed.clone();
            forged[index] ^= 0x01;

            assert!(host.open(&forged).is_err(), "byte {index} went unnoticed");
        }
    }

    #[test]
    fn replayed_datagram_is_rejected() {
        let (client, host) = pair();
        let sealed = client.seal(&datagram(b"once")).unwrap();

        host.open(&sealed).unwrap();

        assert!(matches!(
            host.open(&sealed),
            Err(NetError::SessionReplay { counter: 0 })
        ));
    }

    #[test]
    fn reordered_datagrams_within_the_window_are_accepted() {
        let (client, host) = pair();
        let sealed: Vec<Vec<u8>> = (0..5)
            .map(|index| client.seal(&datagram(&[index])).unwrap())
            .collect();

        for index in [4, 2, 0, 3, 1] {
            assert!(host.open(&sealed[index]).is_ok());
        }
        for datagram in &sealed {
            assert!(matches!(
                host.open(datagram),
                Err(NetError::SessionReplay { .. })
            ));
        }
    }

    #[test]
    fn datagrams_older_than_the_window_are_rejected() {
        let (client, host) = pair();
        let old = client.seal(&datagram(b"old")).unwrap();
        for _ in 0..REPLAY_WINDOW {
            host.open(&client.seal(&datagram(b"filler")).unwrap())
                .unwrap();
        }

        assert!(matches!(
            host.open(&old),
            Err(NetError::SessionReplay { counter: 0 })
        ));
    }

    #[test]
    fn forged_counter_does_not_advance_the_window() {
        let (client, host) = pair();
        let mut forged = client.seal(&datagram(b"x")).unwrap();
        forged[PacketHeader::SIZE..PacketHeader::SIZE + COUNTER_LEN]
            .copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(host.open(&forged).is_err());

        let genuine = client.seal(&datagram(b"y")).unwrap();

        assert!(host.open(&genuine).is_ok());
    }

    #[test]
    fn short_inputs_are_rejected() {
        let (client, host) = pair();

        assert!(matches!(
            client.seal(&[0; 3]),
            Err(NetError::SessionDatagramTooShort { len: 3, .. })
        ));
        assert!(matches!(
            host.open(&[0; PacketHeader::SIZE + SEALED_OVERHEAD - 1]),
            Err(NetError::SessionDatagramTooShort { .. })
        ));
    }

    #[test]
    fn other_sessions_cannot_open_the_datagram() {
        let (client, _) = pair();
        let stranger = SessionCipher::new(&keys(9, 9));

        let sealed = client.seal(&datagram(b"secret")).unwrap();

        assert!(matches!(
            stranger.open(&sealed),
            Err(NetError::SessionAuthentication)
        ));
    }

    #[test]
    fn media_streams_require_sealing_and_handshakes_do_not() {
        assert!(requires_sealing(StreamKind::Video));
        assert!(requires_sealing(StreamKind::Control));
        assert!(requires_sealing(StreamKind::Fec));
        assert!(!requires_sealing(StreamKind::Pairing));
        assert!(!requires_sealing(StreamKind::Reconnect));
        assert!(!requires_sealing(StreamKind::Discovery));
    }

    #[test]
    fn debug_output_hides_key_material() {
        let rendered = format!("{:?} {:?}", keys(0xAB, 0xCD), pair().0);

        assert!(!rendered.contains("171"));
        assert!(!rendered.to_lowercase().contains("ab"));
    }
}
