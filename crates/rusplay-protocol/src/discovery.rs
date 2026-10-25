use crate::{PacketFlags, PacketHeader, ProtocolError, ProtocolVersion, StreamKind};

const TAG_QUERY: u8 = 0;
const TAG_ANNOUNCE: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryMessage {
    Query { nonce: u32 },
    Announce { nonce: u32, port: u16, name: String },
}

impl DiscoveryMessage {
    pub const MAX_NAME_LEN: usize = 64;
    pub const MAX_ENCODED_LEN: usize = PacketHeader::SIZE + 1 + 4 + 2 + 1 + Self::MAX_NAME_LEN;

    pub fn nonce(&self) -> u32 {
        match self {
            Self::Query { nonce } | Self::Announce { nonce, .. } => *nonce,
        }
    }

    pub fn to_datagram(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut payload = Vec::with_capacity(Self::MAX_ENCODED_LEN - PacketHeader::SIZE);

        match self {
            Self::Query { nonce } => {
                payload.push(TAG_QUERY);
                payload.extend_from_slice(&nonce.to_be_bytes());
            }
            Self::Announce { nonce, port, name } => {
                let len = u8::try_from(name.len())
                    .ok()
                    .filter(|len| usize::from(*len) <= Self::MAX_NAME_LEN)
                    .ok_or(ProtocolError::DiscoveryNameTooLong {
                        len: name.len(),
                        max: Self::MAX_NAME_LEN,
                    })?;

                payload.push(TAG_ANNOUNCE);
                payload.extend_from_slice(&nonce.to_be_bytes());
                payload.extend_from_slice(&port.to_be_bytes());
                payload.push(len);
                payload.extend_from_slice(name.as_bytes());
            }
        }

        let header = PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Discovery,
            flags: PacketFlags::empty(),
            sequence: 0,
            frame: 0,
            fragment_index: 0,
            fragment_count: 1,
        };
        let mut datagram = vec![0; PacketHeader::SIZE];
        header.write(&mut datagram)?;
        datagram.extend_from_slice(&payload);

        Ok(datagram)
    }

    pub fn from_datagram(datagram: &[u8]) -> Result<Self, ProtocolError> {
        let (header, payload) = PacketHeader::parse(datagram)?;

        if header.stream != StreamKind::Discovery {
            return Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Discovery,
                found: header.stream,
            });
        }

        let Some((&tag, body)) = payload.split_first() else {
            return Err(ProtocolError::BufferTooShort {
                needed: PacketHeader::SIZE + 1,
                available: datagram.len(),
            });
        };

        match tag {
            TAG_QUERY => {
                let nonce = read_nonce(body)?;
                expect_exact(body, 4)?;
                Ok(Self::Query { nonce })
            }
            TAG_ANNOUNCE => Self::read_announce(body),
            unknown => Err(ProtocolError::UnknownDiscoveryTag(unknown)),
        }
    }

    fn read_announce(body: &[u8]) -> Result<Self, ProtocolError> {
        let nonce = read_nonce(body)?;
        let fixed = body.get(4..7).ok_or(ProtocolError::BufferTooShort {
            needed: 7,
            available: body.len(),
        })?;
        let port = u16::from_be_bytes([fixed[0], fixed[1]]);
        let name_len = usize::from(fixed[2]);

        if name_len > Self::MAX_NAME_LEN {
            return Err(ProtocolError::DiscoveryNameTooLong {
                len: name_len,
                max: Self::MAX_NAME_LEN,
            });
        }

        expect_exact(body, 7 + name_len)?;
        let name = String::from_utf8(body[7..].to_vec())
            .map_err(|_| ProtocolError::InvalidDiscoveryName)?;

        Ok(Self::Announce { nonce, port, name })
    }
}

fn read_nonce(body: &[u8]) -> Result<u32, ProtocolError> {
    body.first_chunk::<4>()
        .map(|bytes| u32::from_be_bytes(*bytes))
        .ok_or(ProtocolError::BufferTooShort {
            needed: 4,
            available: body.len(),
        })
}

fn expect_exact(body: &[u8], expected: usize) -> Result<(), ProtocolError> {
    match body.len().cmp(&expected) {
        std::cmp::Ordering::Less => Err(ProtocolError::BufferTooShort {
            needed: expected,
            available: body.len(),
        }),
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Greater => Err(ProtocolError::TrailingBytes {
            extra: body.len() - expected,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announce(name: &str) -> DiscoveryMessage {
        DiscoveryMessage::Announce {
            nonce: 0xCAFE_F00D,
            port: 47_800,
            name: name.to_owned(),
        }
    }

    #[test]
    fn query_round_trips() {
        let query = DiscoveryMessage::Query { nonce: 7 };

        let datagram = query.to_datagram().unwrap();

        assert_eq!(DiscoveryMessage::from_datagram(&datagram), Ok(query));
    }

    #[test]
    fn announce_round_trips_including_unicode_names() {
        for name in ["", "desk", "Wohnzimmer-PC \u{1F3AE}"] {
            let message = announce(name);

            let datagram = message.to_datagram().unwrap();

            assert_eq!(DiscoveryMessage::from_datagram(&datagram), Ok(message));
        }
    }

    #[test]
    fn announce_layout_is_big_endian() {
        let datagram = announce("ab").to_datagram().unwrap();

        assert_eq!(
            &datagram[PacketHeader::SIZE..],
            [1, 0xCA, 0xFE, 0xF0, 0x0D, 0xBA, 0xB8, 2, b'a', b'b']
        );
    }

    #[test]
    fn longest_name_fits_and_longer_is_refused() {
        let longest = "n".repeat(DiscoveryMessage::MAX_NAME_LEN);
        let datagram = announce(&longest).to_datagram().unwrap();

        assert_eq!(datagram.len(), DiscoveryMessage::MAX_ENCODED_LEN);
        assert_eq!(
            announce(&format!("{longest}x")).to_datagram(),
            Err(ProtocolError::DiscoveryNameTooLong {
                len: DiscoveryMessage::MAX_NAME_LEN + 1,
                max: DiscoveryMessage::MAX_NAME_LEN,
            })
        );
    }

    #[test]
    fn truncated_datagrams_are_rejected() {
        for message in [DiscoveryMessage::Query { nonce: 1 }, announce("host")] {
            let datagram = message.to_datagram().unwrap();

            for cut in 0..datagram.len() {
                assert!(DiscoveryMessage::from_datagram(&datagram[..cut]).is_err());
            }
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut datagram = announce("host").to_datagram().unwrap();
        datagram.push(0);

        assert_eq!(
            DiscoveryMessage::from_datagram(&datagram),
            Err(ProtocolError::TrailingBytes { extra: 1 })
        );
    }

    #[test]
    fn non_utf8_name_is_rejected() {
        let mut datagram = announce("ab").to_datagram().unwrap();
        let len = datagram.len();
        datagram[len - 1] = 0xFF;

        assert_eq!(
            DiscoveryMessage::from_datagram(&datagram),
            Err(ProtocolError::InvalidDiscoveryName)
        );
    }

    #[test]
    fn announced_name_length_beyond_the_limit_is_rejected() {
        let mut datagram = announce("ab").to_datagram().unwrap();
        datagram[PacketHeader::SIZE + 7] = 200;

        assert!(matches!(
            DiscoveryMessage::from_datagram(&datagram),
            Err(ProtocolError::DiscoveryNameTooLong { len: 200, .. })
        ));
    }

    #[test]
    fn unknown_tag_is_rejected() {
        let mut datagram = DiscoveryMessage::Query { nonce: 1 }.to_datagram().unwrap();
        datagram[PacketHeader::SIZE] = 9;

        assert_eq!(
            DiscoveryMessage::from_datagram(&datagram),
            Err(ProtocolError::UnknownDiscoveryTag(9))
        );
    }

    #[test]
    fn other_streams_are_not_discovery() {
        let datagram = crate::ControlPacket::Ack { id: 1 }.to_datagram().unwrap();

        assert_eq!(
            DiscoveryMessage::from_datagram(&datagram),
            Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Discovery,
                found: StreamKind::Control,
            })
        );
    }

    #[test]
    fn nonce_is_exposed_for_both_variants() {
        assert_eq!(DiscoveryMessage::Query { nonce: 3 }.nonce(), 3);
        assert_eq!(announce("x").nonce(), 0xCAFE_F00D);
    }
}
