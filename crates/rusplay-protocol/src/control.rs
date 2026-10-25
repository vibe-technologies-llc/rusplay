use crate::{
    PacketFlags, PacketHeader, ProtocolError, ProtocolVersion, StreamDescription, StreamKind,
    stream::Reader,
};

const TAG_HELLO: u8 = 0;
const TAG_REQUEST_KEYFRAME: u8 = 1;
const TAG_REPORT_LOSS: u8 = 2;
const TAG_BITRATE_HINT: u8 = 3;
const TAG_GOODBYE: u8 = 4;
const TAG_KEEPALIVE: u8 = 5;
const TAG_STREAM_FORMAT: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMessage {
    Hello { version: ProtocolVersion },
    RequestKeyframe { frame: u32 },
    ReportLoss { first_sequence: u32, count: u16 },
    BitrateHint { kbps: u32 },
    Goodbye,
    Keepalive,
    StreamFormat(StreamDescription),
}

impl ControlMessage {
    pub const MAX_ENCODED_LEN: usize = 1 + StreamDescription::MAX_ENCODED_LEN;

    pub const fn encoded_len(&self) -> usize {
        match self {
            Self::Hello { .. } => 3,
            Self::RequestKeyframe { .. } | Self::BitrateHint { .. } => 5,
            Self::ReportLoss { .. } => 7,
            Self::Goodbye | Self::Keepalive => 1,
            Self::StreamFormat(description) => 1 + description.encoded_len(),
        }
    }

    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        let len = self.encoded_len();
        let available = buf.len();
        let Some(out) = buf.get_mut(..len) else {
            return Err(ProtocolError::BufferTooShort {
                needed: len,
                available,
            });
        };

        match self {
            Self::Hello { version } => {
                out[0] = TAG_HELLO;
                out[1..3].copy_from_slice(&version.0.to_be_bytes());
            }
            Self::RequestKeyframe { frame } => {
                out[0] = TAG_REQUEST_KEYFRAME;
                out[1..5].copy_from_slice(&frame.to_be_bytes());
            }
            Self::ReportLoss {
                first_sequence,
                count,
            } => {
                out[0] = TAG_REPORT_LOSS;
                out[1..5].copy_from_slice(&first_sequence.to_be_bytes());
                out[5..7].copy_from_slice(&count.to_be_bytes());
            }
            Self::BitrateHint { kbps } => {
                out[0] = TAG_BITRATE_HINT;
                out[1..5].copy_from_slice(&kbps.to_be_bytes());
            }
            Self::Goodbye => out[0] = TAG_GOODBYE,
            Self::Keepalive => out[0] = TAG_KEEPALIVE,
            Self::StreamFormat(description) => {
                let mut body = Vec::with_capacity(len);
                body.push(TAG_STREAM_FORMAT);
                description.write(&mut body);
                out.copy_from_slice(&body);
            }
        }

        Ok(len)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let Some((&tag, body)) = buf.split_first() else {
            return Err(ProtocolError::BufferTooShort {
                needed: 1,
                available: 0,
            });
        };

        if tag == TAG_STREAM_FORMAT {
            let mut reader = Reader::new(body, 1);
            let description = StreamDescription::read(&mut reader)?;
            return match reader.remaining() {
                0 => Ok(Self::StreamFormat(description)),
                extra => Err(ProtocolError::TrailingBytes { extra }),
            };
        }

        let body_len = match tag {
            TAG_HELLO => 2,
            TAG_REQUEST_KEYFRAME | TAG_BITRATE_HINT => 4,
            TAG_REPORT_LOSS => 6,
            TAG_GOODBYE | TAG_KEEPALIVE => 0,
            unknown => return Err(ProtocolError::UnknownControlTag(unknown)),
        };

        if body.len() < body_len {
            return Err(ProtocolError::BufferTooShort {
                needed: 1 + body_len,
                available: buf.len(),
            });
        }

        if body.len() > body_len {
            return Err(ProtocolError::TrailingBytes {
                extra: body.len() - body_len,
            });
        }

        let word =
            |at: usize| u32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);

        Ok(match tag {
            TAG_HELLO => Self::Hello {
                version: ProtocolVersion(u16::from_be_bytes([body[0], body[1]])),
            },
            TAG_REQUEST_KEYFRAME => Self::RequestKeyframe { frame: word(0) },
            TAG_REPORT_LOSS => Self::ReportLoss {
                first_sequence: word(0),
                count: u16::from_be_bytes([body[4], body[5]]),
            },
            TAG_BITRATE_HINT => Self::BitrateHint { kbps: word(0) },
            TAG_KEEPALIVE => Self::Keepalive,
            _ => Self::Goodbye,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPacket {
    Message { id: u32, message: ControlMessage },
    Ack { id: u32 },
}

impl ControlPacket {
    pub const MAX_ENCODED_LEN: usize = PacketHeader::SIZE + ControlMessage::MAX_ENCODED_LEN;

    pub const fn id(&self) -> u32 {
        match self {
            Self::Message { id, .. } | Self::Ack { id } => *id,
        }
    }

    pub fn write(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        let flags = match self {
            Self::Message { .. } => PacketFlags::empty(),
            Self::Ack { .. } => PacketFlags::ACK,
        };
        let header = PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Control,
            flags,
            sequence: self.id(),
            frame: 0,
            fragment_index: 0,
            fragment_count: 1,
        };

        header.write(buf)?;
        match self {
            Self::Message { message, .. } => {
                Ok(PacketHeader::SIZE + message.encode(&mut buf[PacketHeader::SIZE..])?)
            }
            Self::Ack { .. } => Ok(PacketHeader::SIZE),
        }
    }

    pub fn to_datagram(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut buf = [0; Self::MAX_ENCODED_LEN];
        let len = self.write(&mut buf)?;

        Ok(buf[..len].to_vec())
    }

    pub fn read(datagram: &[u8]) -> Result<Self, ProtocolError> {
        let (header, payload) = PacketHeader::parse(datagram)?;

        if header.stream != StreamKind::Control {
            return Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Control,
                found: header.stream,
            });
        }

        if header.flags.contains(PacketFlags::ACK) {
            if !payload.is_empty() {
                return Err(ProtocolError::TrailingBytes {
                    extra: payload.len(),
                });
            }
            return Ok(Self::Ack {
                id: header.sequence,
            });
        }

        Ok(Self::Message {
            id: header.sequence,
            message: ControlMessage::decode(payload)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_message() -> [ControlMessage; 7] {
        [
            ControlMessage::Hello {
                version: ProtocolVersion::CURRENT,
            },
            ControlMessage::RequestKeyframe { frame: 0xDEAD_BEEF },
            ControlMessage::ReportLoss {
                first_sequence: u32::MAX - 3,
                count: 513,
            },
            ControlMessage::BitrateHint { kbps: 150_000 },
            ControlMessage::Goodbye,
            ControlMessage::Keepalive,
            ControlMessage::StreamFormat(crate::stream::tests::hdr_description()),
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for message in every_message() {
            let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];

            let len = message.encode(&mut buf).unwrap();

            assert_eq!(len, message.encoded_len());
            assert_eq!(ControlMessage::decode(&buf[..len]), Ok(message));
        }
    }

    #[test]
    fn max_encoded_len_covers_every_message() {
        let longest = every_message()
            .iter()
            .map(ControlMessage::encoded_len)
            .max()
            .unwrap();

        assert_eq!(longest, ControlMessage::MAX_ENCODED_LEN);
    }

    #[test]
    fn encoding_is_big_endian_with_a_leading_tag() {
        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];

        let len = ControlMessage::ReportLoss {
            first_sequence: 0x0102_0304,
            count: 0x0506,
        }
        .encode(&mut buf)
        .unwrap();

        assert_eq!(&buf[..len], [2, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn short_output_buffer_is_rejected() {
        assert_eq!(
            ControlMessage::RequestKeyframe { frame: 1 }.encode(&mut [0; 4]),
            Err(ProtocolError::BufferTooShort {
                needed: 5,
                available: 4,
            })
        );
    }

    #[test]
    fn truncated_input_is_rejected_for_every_message() {
        for message in every_message() {
            let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];
            let len = message.encode(&mut buf).unwrap();

            for cut in 0..len {
                assert!(matches!(
                    ControlMessage::decode(&buf[..cut]),
                    Err(ProtocolError::BufferTooShort { .. })
                ));
            }
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        assert_eq!(
            ControlMessage::decode(&[TAG_GOODBYE, 0, 0]),
            Err(ProtocolError::TrailingBytes { extra: 2 })
        );
    }

    #[test]
    fn unknown_tag_is_rejected() {
        assert_eq!(
            ControlMessage::decode(&[0xEE]),
            Err(ProtocolError::UnknownControlTag(0xEE))
        );
    }

    #[test]
    fn message_packet_round_trips_through_a_datagram() {
        for (id, message) in every_message().into_iter().enumerate() {
            let packet = ControlPacket::Message {
                id: (u32::MAX - 1).wrapping_add(u32::try_from(id).unwrap()),
                message,
            };

            let datagram = packet.to_datagram().unwrap();

            assert_eq!(ControlPacket::read(&datagram), Ok(packet));
        }
    }

    #[test]
    fn ack_packet_is_header_only() {
        let packet = ControlPacket::Ack { id: 77 };

        let datagram = packet.to_datagram().unwrap();

        assert_eq!(datagram.len(), PacketHeader::SIZE);
        assert_eq!(ControlPacket::read(&datagram), Ok(packet));
    }

    #[test]
    fn ack_with_payload_is_rejected() {
        let mut datagram = ControlPacket::Ack { id: 1 }.to_datagram().unwrap();
        datagram.push(0);

        assert_eq!(
            ControlPacket::read(&datagram),
            Err(ProtocolError::TrailingBytes { extra: 1 })
        );
    }

    #[test]
    fn media_datagram_is_not_a_control_packet() {
        let header = PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Video,
            flags: PacketFlags::empty(),
            sequence: 0,
            frame: 0,
            fragment_index: 0,
            fragment_count: 1,
        };
        let mut datagram = [0; PacketHeader::SIZE];
        header.write(&mut datagram).unwrap();

        assert_eq!(
            ControlPacket::read(&datagram),
            Err(ProtocolError::UnexpectedStream {
                expected: StreamKind::Control,
                found: StreamKind::Video,
            })
        );
    }

    #[test]
    fn message_packet_without_a_body_is_rejected() {
        let mut datagram = ControlPacket::Message {
            id: 0,
            message: ControlMessage::Goodbye,
        }
        .to_datagram()
        .unwrap();
        datagram.pop();

        assert!(matches!(
            ControlPacket::read(&datagram),
            Err(ProtocolError::BufferTooShort { .. })
        ));
    }
}
