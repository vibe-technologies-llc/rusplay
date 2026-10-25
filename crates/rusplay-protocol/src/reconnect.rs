use std::fmt;

use crate::{PacketFlags, PacketHeader, ProtocolError, ProtocolVersion, StreamKind};

const TAG_HELLO: u8 = 0;
const TAG_CHALLENGE: u8 = 1;
const TAG_PROOF: u8 = 2;
const TAG_ACCEPTED: u8 = 3;
const TAG_REJECTED: u8 = 4;

const TAG_AND_ATTEMPT_LEN: usize = 1 + 4;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerId(pub [u8; PeerId::LEN]);

impl PeerId {
    pub const LEN: usize = 16;
}

impl fmt::Debug for PeerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReconnectRejection {
    UnknownPeer = 0,
    Authentication = 1,
}

impl TryFrom<u8> for ReconnectRejection {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::UnknownPeer),
            1 => Ok(Self::Authentication),
            unknown => Err(ProtocolError::UnknownReconnectRejection(unknown)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectMessage {
    Hello {
        attempt: u32,
        peer: PeerId,
        nonce: [u8; ReconnectMessage::NONCE_LEN],
    },
    Challenge {
        attempt: u32,
        nonce: [u8; ReconnectMessage::NONCE_LEN],
        proof: [u8; ReconnectMessage::PROOF_LEN],
    },
    Proof {
        attempt: u32,
        proof: [u8; ReconnectMessage::PROOF_LEN],
    },
    Accepted {
        attempt: u32,
    },
    Rejected {
        attempt: u32,
        reason: ReconnectRejection,
    },
}

impl ReconnectMessage {
    pub const NONCE_LEN: usize = 32;
    pub const PROOF_LEN: usize = 32;
    pub const MAX_ENCODED_LEN: usize =
        PacketHeader::SIZE + TAG_AND_ATTEMPT_LEN + PeerId::LEN + Self::NONCE_LEN + Self::PROOF_LEN;

    pub const fn attempt(&self) -> u32 {
        match self {
            Self::Hello { attempt, .. }
            | Self::Challenge { attempt, .. }
            | Self::Proof { attempt, .. }
            | Self::Accepted { attempt }
            | Self::Rejected { attempt, .. } => *attempt,
        }
    }

    pub fn to_datagram(&self) -> Result<Vec<u8>, ProtocolError> {
        let header = PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Reconnect,
            flags: PacketFlags::empty(),
            sequence: 0,
            frame: 0,
            fragment_index: 0,
            fragment_count: 1,
        };
        let mut datagram = vec![0; PacketHeader::SIZE];
        header.write(&mut datagram)?;

        let tag = match self {
            Self::Hello { .. } => TAG_HELLO,
            Self::Challenge { .. } => TAG_CHALLENGE,
            Self::Proof { .. } => TAG_PROOF,
            Self::Accepted { .. } => TAG_ACCEPTED,
            Self::Rejected { .. } => TAG_REJECTED,
        };
        datagram.push(tag);
        datagram.extend_from_slice(&self.attempt().to_be_bytes());

        match self {
            Self::Hello { peer, nonce, .. } => {
                datagram.extend_from_slice(&peer.0);
                datagram.extend_from_slice(nonce);
            }
            Self::Challenge { nonce, proof, .. } => {
                datagram.extend_from_slice(nonce);
                datagram.extend_from_slice(proof);
            }
            Self::Proof { proof, .. } => datagram.extend_from_slice(proof),
            Self::Accepted { .. } => {}
            Self::Rejected { reason, .. } => datagram.push(*reason as u8),
        }

        Ok(datagram)
    }

    pub fn from_datagram(datagram: &[u8]) -> Result<Self, ProtocolError> {
        let (header, payload) = PacketHeader::parse(datagram)?;

        if header.stream != StreamKind::Reconnect {
            return Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Reconnect,
                found: header.stream,
            });
        }

        let Some((&tag, rest)) = payload.split_first() else {
            return Err(ProtocolError::BufferTooShort {
                needed: PacketHeader::SIZE + 1,
                available: datagram.len(),
            });
        };
        let Some((attempt_bytes, body)) = rest.split_first_chunk::<4>() else {
            return Err(ProtocolError::BufferTooShort {
                needed: PacketHeader::SIZE + TAG_AND_ATTEMPT_LEN,
                available: datagram.len(),
            });
        };
        let attempt = u32::from_be_bytes(*attempt_bytes);

        let body_len = match tag {
            TAG_HELLO => PeerId::LEN + Self::NONCE_LEN,
            TAG_CHALLENGE => Self::NONCE_LEN + Self::PROOF_LEN,
            TAG_PROOF => Self::PROOF_LEN,
            TAG_ACCEPTED => 0,
            TAG_REJECTED => 1,
            unknown => return Err(ProtocolError::UnknownReconnectTag(unknown)),
        };

        if body.len() < body_len {
            return Err(ProtocolError::BufferTooShort {
                needed: PacketHeader::SIZE + TAG_AND_ATTEMPT_LEN + body_len,
                available: datagram.len(),
            });
        }

        if body.len() > body_len {
            return Err(ProtocolError::TrailingBytes {
                extra: body.len() - body_len,
            });
        }

        Ok(match tag {
            TAG_HELLO => Self::Hello {
                attempt,
                peer: PeerId(fixed(&body[..PeerId::LEN])),
                nonce: fixed(&body[PeerId::LEN..]),
            },
            TAG_CHALLENGE => Self::Challenge {
                attempt,
                nonce: fixed(&body[..Self::NONCE_LEN]),
                proof: fixed(&body[Self::NONCE_LEN..]),
            },
            TAG_PROOF => Self::Proof {
                attempt,
                proof: fixed(body),
            },
            TAG_ACCEPTED => Self::Accepted { attempt },
            _ => Self::Rejected {
                attempt,
                reason: ReconnectRejection::try_from(body[0])?,
            },
        })
    }
}

fn fixed<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0; N];
    out.copy_from_slice(bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_message() -> [ReconnectMessage; 5] {
        [
            ReconnectMessage::Hello {
                attempt: 0xA1B2_C3D4,
                peer: PeerId([5; PeerId::LEN]),
                nonce: [6; ReconnectMessage::NONCE_LEN],
            },
            ReconnectMessage::Challenge {
                attempt: 1,
                nonce: [8; ReconnectMessage::NONCE_LEN],
                proof: [9; ReconnectMessage::PROOF_LEN],
            },
            ReconnectMessage::Proof {
                attempt: 2,
                proof: [10; ReconnectMessage::PROOF_LEN],
            },
            ReconnectMessage::Accepted { attempt: 3 },
            ReconnectMessage::Rejected {
                attempt: 4,
                reason: ReconnectRejection::UnknownPeer,
            },
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for message in every_message() {
            let datagram = message.to_datagram().unwrap();

            assert_eq!(ReconnectMessage::from_datagram(&datagram), Ok(message));
            assert!(datagram.len() <= ReconnectMessage::MAX_ENCODED_LEN);
        }
    }

    #[test]
    fn every_rejection_reason_round_trips() {
        for reason in [
            ReconnectRejection::UnknownPeer,
            ReconnectRejection::Authentication,
        ] {
            let message = ReconnectMessage::Rejected { attempt: 9, reason };
            let datagram = message.to_datagram().unwrap();

            assert_eq!(ReconnectMessage::from_datagram(&datagram), Ok(message));
        }
    }

    #[test]
    fn attempt_is_exposed_for_every_variant() {
        let attempts: Vec<u32> = every_message()
            .iter()
            .map(ReconnectMessage::attempt)
            .collect();

        assert_eq!(attempts, [0xA1B2_C3D4, 1, 2, 3, 4]);
    }

    #[test]
    fn truncated_datagrams_are_rejected() {
        for message in every_message() {
            let datagram = message.to_datagram().unwrap();

            for cut in 0..datagram.len() {
                assert!(ReconnectMessage::from_datagram(&datagram[..cut]).is_err());
            }
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        for message in every_message() {
            let mut datagram = message.to_datagram().unwrap();
            datagram.push(0);

            assert_eq!(
                ReconnectMessage::from_datagram(&datagram),
                Err(ProtocolError::TrailingBytes { extra: 1 })
            );
        }
    }

    #[test]
    fn unknown_tag_and_rejection_are_rejected() {
        let mut datagram = every_message()[3].to_datagram().unwrap();
        datagram[PacketHeader::SIZE] = 99;
        assert_eq!(
            ReconnectMessage::from_datagram(&datagram),
            Err(ProtocolError::UnknownReconnectTag(99))
        );

        let mut rejected = every_message()[4].to_datagram().unwrap();
        let last = rejected.len() - 1;
        rejected[last] = 77;
        assert_eq!(
            ReconnectMessage::from_datagram(&rejected),
            Err(ProtocolError::UnknownReconnectRejection(77))
        );
    }

    #[test]
    fn pairing_datagrams_are_not_reconnect() {
        let datagram = crate::PairingMessage::Done { attempt: 1 }
            .to_datagram()
            .unwrap();

        assert_eq!(
            ReconnectMessage::from_datagram(&datagram),
            Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Reconnect,
                found: StreamKind::Pairing,
            })
        );
    }

    #[test]
    fn reconnect_datagrams_are_not_pairing() {
        let datagram = every_message()[3].to_datagram().unwrap();

        assert_eq!(
            crate::PairingMessage::from_datagram(&datagram),
            Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Pairing,
                found: StreamKind::Reconnect,
            })
        );
    }

    #[test]
    fn peer_id_displays_as_hex() {
        let id = PeerId([0xAB; PeerId::LEN]);

        assert_eq!(id.to_string(), "ab".repeat(PeerId::LEN));
        assert_eq!(format!("{id:?}"), id.to_string());
    }
}
