use crate::{PacketFlags, PacketHeader, ProtocolError, ProtocolVersion, StreamKind};

const TAG_START: u8 = 0;
const TAG_REPLY: u8 = 1;
const TAG_CONFIRM: u8 = 2;
const TAG_DONE: u8 = 3;
const TAG_REJECTED: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PairingRejection {
    WrongPin = 0,
    Locked = 1,
    Busy = 2,
}

impl TryFrom<u8> for PairingRejection {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::WrongPin),
            1 => Ok(Self::Locked),
            2 => Ok(Self::Busy),
            unknown => Err(ProtocolError::UnknownPairingRejection(unknown)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingMessage {
    Start {
        attempt: u32,
        spake: [u8; PairingMessage::SPAKE_LEN],
    },
    Reply {
        attempt: u32,
        spake: [u8; PairingMessage::SPAKE_LEN],
        confirm: [u8; PairingMessage::MAC_LEN],
    },
    Confirm {
        attempt: u32,
        confirm: [u8; PairingMessage::MAC_LEN],
    },
    Done {
        attempt: u32,
    },
    Rejected {
        attempt: u32,
        reason: PairingRejection,
    },
}

impl PairingMessage {
    pub const SPAKE_LEN: usize = 33;
    pub const MAC_LEN: usize = 32;
    pub const MAX_ENCODED_LEN: usize = PacketHeader::SIZE + 1 + 4 + Self::SPAKE_LEN + Self::MAC_LEN;

    pub const fn attempt(&self) -> u32 {
        match self {
            Self::Start { attempt, .. }
            | Self::Reply { attempt, .. }
            | Self::Confirm { attempt, .. }
            | Self::Done { attempt }
            | Self::Rejected { attempt, .. } => *attempt,
        }
    }

    pub fn to_datagram(&self) -> Result<Vec<u8>, ProtocolError> {
        let header = PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Pairing,
            flags: PacketFlags::empty(),
            sequence: 0,
            frame: 0,
            fragment_index: 0,
            fragment_count: 1,
        };
        let mut datagram = vec![0; PacketHeader::SIZE];
        header.write(&mut datagram)?;

        let (tag, attempt) = match self {
            Self::Start { attempt, .. } => (TAG_START, *attempt),
            Self::Reply { attempt, .. } => (TAG_REPLY, *attempt),
            Self::Confirm { attempt, .. } => (TAG_CONFIRM, *attempt),
            Self::Done { attempt } => (TAG_DONE, *attempt),
            Self::Rejected { attempt, .. } => (TAG_REJECTED, *attempt),
        };
        datagram.push(tag);
        datagram.extend_from_slice(&attempt.to_be_bytes());

        match self {
            Self::Start { spake, .. } => datagram.extend_from_slice(spake),
            Self::Reply { spake, confirm, .. } => {
                datagram.extend_from_slice(spake);
                datagram.extend_from_slice(confirm);
            }
            Self::Confirm { confirm, .. } => datagram.extend_from_slice(confirm),
            Self::Done { .. } => {}
            Self::Rejected { reason, .. } => datagram.push(*reason as u8),
        }

        Ok(datagram)
    }

    pub fn from_datagram(datagram: &[u8]) -> Result<Self, ProtocolError> {
        let (header, payload) = PacketHeader::parse(datagram)?;

        if header.stream != StreamKind::Pairing {
            return Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Pairing,
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
                needed: PacketHeader::SIZE + 5,
                available: datagram.len(),
            });
        };
        let attempt = u32::from_be_bytes(*attempt_bytes);

        let body_len = match tag {
            TAG_START => Self::SPAKE_LEN,
            TAG_REPLY => Self::SPAKE_LEN + Self::MAC_LEN,
            TAG_CONFIRM => Self::MAC_LEN,
            TAG_DONE => 0,
            TAG_REJECTED => 1,
            unknown => return Err(ProtocolError::UnknownPairingTag(unknown)),
        };

        if body.len() < body_len {
            return Err(ProtocolError::BufferTooShort {
                needed: PacketHeader::SIZE + 5 + body_len,
                available: datagram.len(),
            });
        }

        if body.len() > body_len {
            return Err(ProtocolError::TrailingBytes {
                extra: body.len() - body_len,
            });
        }

        Ok(match tag {
            TAG_START => Self::Start {
                attempt,
                spake: fixed(body),
            },
            TAG_REPLY => Self::Reply {
                attempt,
                spake: fixed(&body[..Self::SPAKE_LEN]),
                confirm: fixed(&body[Self::SPAKE_LEN..]),
            },
            TAG_CONFIRM => Self::Confirm {
                attempt,
                confirm: fixed(body),
            },
            TAG_DONE => Self::Done { attempt },
            _ => Self::Rejected {
                attempt,
                reason: PairingRejection::try_from(body[0])?,
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

    fn every_message() -> [PairingMessage; 5] {
        [
            PairingMessage::Start {
                attempt: 0xA1B2_C3D4,
                spake: [7; PairingMessage::SPAKE_LEN],
            },
            PairingMessage::Reply {
                attempt: 1,
                spake: [8; PairingMessage::SPAKE_LEN],
                confirm: [9; PairingMessage::MAC_LEN],
            },
            PairingMessage::Confirm {
                attempt: 2,
                confirm: [10; PairingMessage::MAC_LEN],
            },
            PairingMessage::Done { attempt: 3 },
            PairingMessage::Rejected {
                attempt: 4,
                reason: PairingRejection::Locked,
            },
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for message in every_message() {
            let datagram = message.to_datagram().unwrap();

            assert_eq!(PairingMessage::from_datagram(&datagram), Ok(message));
            assert!(datagram.len() <= PairingMessage::MAX_ENCODED_LEN);
        }
    }

    #[test]
    fn every_rejection_reason_round_trips() {
        for reason in [
            PairingRejection::WrongPin,
            PairingRejection::Locked,
            PairingRejection::Busy,
        ] {
            let message = PairingMessage::Rejected { attempt: 9, reason };
            let datagram = message.to_datagram().unwrap();

            assert_eq!(PairingMessage::from_datagram(&datagram), Ok(message));
        }
    }

    #[test]
    fn attempt_is_exposed_for_every_variant() {
        let attempts: Vec<u32> = every_message()
            .iter()
            .map(PairingMessage::attempt)
            .collect();

        assert_eq!(attempts, [0xA1B2_C3D4, 1, 2, 3, 4]);
    }

    #[test]
    fn truncated_datagrams_are_rejected() {
        for message in every_message() {
            let datagram = message.to_datagram().unwrap();

            for cut in 0..datagram.len() {
                assert!(PairingMessage::from_datagram(&datagram[..cut]).is_err());
            }
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        for message in every_message() {
            let mut datagram = message.to_datagram().unwrap();
            datagram.push(0);

            assert_eq!(
                PairingMessage::from_datagram(&datagram),
                Err(ProtocolError::TrailingBytes { extra: 1 })
            );
        }
    }

    #[test]
    fn unknown_tag_and_rejection_are_rejected() {
        let mut datagram = PairingMessage::Done { attempt: 0 }.to_datagram().unwrap();
        datagram[PacketHeader::SIZE] = 99;
        assert_eq!(
            PairingMessage::from_datagram(&datagram),
            Err(ProtocolError::UnknownPairingTag(99))
        );

        let mut rejected = every_message()[4].to_datagram().unwrap();
        let last = rejected.len() - 1;
        rejected[last] = 77;
        assert_eq!(
            PairingMessage::from_datagram(&rejected),
            Err(ProtocolError::UnknownPairingRejection(77))
        );
    }

    #[test]
    fn other_streams_are_not_pairing() {
        let datagram = crate::ControlPacket::Ack { id: 1 }.to_datagram().unwrap();

        assert_eq!(
            PairingMessage::from_datagram(&datagram),
            Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Pairing,
                found: StreamKind::Control,
            })
        );
    }
}
