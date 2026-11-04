use rusplay_core::FormatSet;

use crate::{
    ClientDisplay, PacketFlags, PacketHeader, ProtocolError, ProtocolVersion, StreamDescription,
    StreamKind, stream::Reader,
};

const TAG_HELLO: u8 = 0;
const TAG_REQUEST_KEYFRAME: u8 = 1;
const TAG_REPORT_LOSS: u8 = 2;
const TAG_BITRATE_HINT: u8 = 3;
const TAG_GOODBYE: u8 = 4;
const TAG_KEEPALIVE: u8 = 5;
const TAG_RUMBLE: u8 = 6;
const TAG_STREAM_FORMAT: u8 = 7;
const TAG_CLOCK_PING: u8 = 8;
const TAG_CLOCK_PONG: u8 = 9;
const TAG_ACKNOWLEDGE_FRAME: u8 = 10;
const TAG_REQUEST_RECOVERY: u8 = 11;
const TAG_LIGHTBAR: u8 = 12;
const TAG_PLAYER_LEDS: u8 = 13;
const TAG_TRIGGER_EFFECT: u8 = 14;
const TAG_DISPLAY: u8 = 15;

pub const TRIGGER_EFFECT_LEN: usize = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMessage {
    Hello {
        version: ProtocolVersion,
        formats: FormatSet,
    },
    RequestKeyframe {
        frame: u32,
    },
    ReportLoss {
        first_sequence: u32,
        count: u16,
    },
    BitrateHint {
        kbps: u32,
    },
    Goodbye,
    Keepalive,
    Rumble {
        slot: u8,
        strong: u16,
        weak: u16,
        delay_ms: u16,
        duration_ms: u16,
    },
    StreamFormat(StreamDescription),
    ClockPing {
        sent_micros: u64,
    },
    ClockPong {
        sent_micros: u64,
        host_micros: u64,
    },
    AcknowledgeFrame {
        frame: u32,
    },
    RequestRecovery {
        frame: u32,
        last_good: u32,
    },
    Lightbar {
        slot: u8,
        red: u8,
        green: u8,
        blue: u8,
    },
    PlayerLeds {
        slot: u8,
        mask: u8,
    },
    TriggerEffect {
        slot: u8,
        right: bool,
        effect: [u8; TRIGGER_EFFECT_LEN],
    },
    Display(ClientDisplay),
}

impl ControlMessage {
    pub const MAX_ENCODED_LEN: usize = 1 + StreamDescription::MAX_ENCODED_LEN;

    pub const fn encoded_len(&self) -> usize {
        match self {
            Self::RequestKeyframe { .. }
            | Self::BitrateHint { .. }
            | Self::AcknowledgeFrame { .. }
            | Self::Lightbar { .. } => 5,
            Self::Hello { .. } | Self::ReportLoss { .. } => 7,
            Self::Goodbye | Self::Keepalive => 1,
            Self::Rumble { .. } => 10,
            Self::StreamFormat(description) => 1 + description.encoded_len(),
            Self::Display(display) => 1 + display.encoded_len(),
            Self::ClockPing { .. } | Self::RequestRecovery { .. } => 9,
            Self::ClockPong { .. } => 17,
            Self::PlayerLeds { .. } => 3,
            Self::TriggerEffect { .. } => 3 + TRIGGER_EFFECT_LEN,
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
            Self::Hello { version, formats } => {
                out[0] = TAG_HELLO;
                out[1..3].copy_from_slice(&version.0.to_be_bytes());
                out[3..7].copy_from_slice(&formats.bits().to_be_bytes());
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
            Self::Rumble {
                slot,
                strong,
                weak,
                delay_ms,
                duration_ms,
            } => {
                out[0] = TAG_RUMBLE;
                out[1] = *slot;
                out[2..4].copy_from_slice(&strong.to_be_bytes());
                out[4..6].copy_from_slice(&weak.to_be_bytes());
                out[6..8].copy_from_slice(&delay_ms.to_be_bytes());
                out[8..10].copy_from_slice(&duration_ms.to_be_bytes());
            }
            Self::ClockPing { sent_micros } => {
                out[0] = TAG_CLOCK_PING;
                out[1..9].copy_from_slice(&sent_micros.to_be_bytes());
            }
            Self::ClockPong {
                sent_micros,
                host_micros,
            } => {
                out[0] = TAG_CLOCK_PONG;
                out[1..9].copy_from_slice(&sent_micros.to_be_bytes());
                out[9..17].copy_from_slice(&host_micros.to_be_bytes());
            }
            Self::AcknowledgeFrame { frame } => {
                out[0] = TAG_ACKNOWLEDGE_FRAME;
                out[1..5].copy_from_slice(&frame.to_be_bytes());
            }
            Self::RequestRecovery { frame, last_good } => {
                out[0] = TAG_REQUEST_RECOVERY;
                out[1..5].copy_from_slice(&frame.to_be_bytes());
                out[5..9].copy_from_slice(&last_good.to_be_bytes());
            }
            Self::Lightbar {
                slot,
                red,
                green,
                blue,
            } => out.copy_from_slice(&[TAG_LIGHTBAR, *slot, *red, *green, *blue]),
            Self::PlayerLeds { slot, mask } => {
                out.copy_from_slice(&[TAG_PLAYER_LEDS, *slot, *mask]);
            }
            Self::TriggerEffect {
                slot,
                right,
                effect,
            } => {
                out[0] = TAG_TRIGGER_EFFECT;
                out[1] = *slot;
                out[2] = u8::from(*right);
                out[3..].copy_from_slice(effect);
            }
            Self::StreamFormat(description) => {
                let mut body = Vec::with_capacity(len);
                body.push(TAG_STREAM_FORMAT);
                description.write(&mut body);
                out.copy_from_slice(&body);
            }
            Self::Display(display) => {
                let mut body = Vec::with_capacity(len);
                body.push(TAG_DISPLAY);
                display.write(&mut body);
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

        if let Some(decoded) = Self::decode_pad_feedback(tag, body) {
            return decoded;
        }

        if tag == TAG_DISPLAY {
            let mut reader = Reader::new(body, 1);
            let display = ClientDisplay::read(&mut reader)?;
            return match reader.remaining() {
                0 => Ok(Self::Display(display)),
                extra => Err(ProtocolError::TrailingBytes { extra }),
            };
        }

        let body_len = match tag {
            TAG_REQUEST_KEYFRAME | TAG_BITRATE_HINT | TAG_ACKNOWLEDGE_FRAME => 4,
            TAG_HELLO | TAG_REPORT_LOSS => 6,
            TAG_GOODBYE | TAG_KEEPALIVE => 0,
            TAG_RUMBLE => 9,
            TAG_CLOCK_PING | TAG_REQUEST_RECOVERY => 8,
            TAG_CLOCK_PONG => 16,
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

        let long = |at: usize| {
            u64::from_be_bytes([
                body[at],
                body[at + 1],
                body[at + 2],
                body[at + 3],
                body[at + 4],
                body[at + 5],
                body[at + 6],
                body[at + 7],
            ])
        };

        Ok(match tag {
            TAG_HELLO => {
                let bits = word(2);
                Self::Hello {
                    version: ProtocolVersion(u16::from_be_bytes([body[0], body[1]])),
                    formats: FormatSet::from_bits(bits)
                        .ok_or(ProtocolError::UnknownFormatBits(bits))?,
                }
            }
            TAG_REQUEST_KEYFRAME => Self::RequestKeyframe { frame: word(0) },
            TAG_REPORT_LOSS => Self::ReportLoss {
                first_sequence: word(0),
                count: u16::from_be_bytes([body[4], body[5]]),
            },
            TAG_BITRATE_HINT => Self::BitrateHint { kbps: word(0) },
            TAG_ACKNOWLEDGE_FRAME => Self::AcknowledgeFrame { frame: word(0) },
            TAG_REQUEST_RECOVERY => Self::RequestRecovery {
                frame: word(0),
                last_good: word(4),
            },
            TAG_KEEPALIVE => Self::Keepalive,
            TAG_CLOCK_PING => Self::ClockPing {
                sent_micros: long(0),
            },
            TAG_CLOCK_PONG => Self::ClockPong {
                sent_micros: long(0),
                host_micros: long(8),
            },
            TAG_RUMBLE => {
                let half = |at: usize| u16::from_be_bytes([body[at], body[at + 1]]);
                Self::Rumble {
                    slot: body[0],
                    strong: half(1),
                    weak: half(3),
                    delay_ms: half(5),
                    duration_ms: half(7),
                }
            }
            _ => Self::Goodbye,
        })
    }
}

impl ControlMessage {
    fn decode_pad_feedback(tag: u8, body: &[u8]) -> Option<Result<Self, ProtocolError>> {
        let expected = match tag {
            TAG_LIGHTBAR => 4,
            TAG_PLAYER_LEDS => 2,
            TAG_TRIGGER_EFFECT => 2 + TRIGGER_EFFECT_LEN,
            _ => return None,
        };

        if body.len() < expected {
            return Some(Err(ProtocolError::BufferTooShort {
                needed: 1 + expected,
                available: 1 + body.len(),
            }));
        }

        if body.len() > expected {
            return Some(Err(ProtocolError::TrailingBytes {
                extra: body.len() - expected,
            }));
        }

        Some(match tag {
            TAG_LIGHTBAR => Ok(Self::Lightbar {
                slot: body[0],
                red: body[1],
                green: body[2],
                blue: body[3],
            }),
            TAG_PLAYER_LEDS => Ok(Self::PlayerLeds {
                slot: body[0],
                mask: body[1],
            }),
            _ => Self::decode_trigger_effect(body),
        })
    }

    fn decode_trigger_effect(body: &[u8]) -> Result<Self, ProtocolError> {
        let right = match body[1] {
            0 => false,
            1 => true,
            other => return Err(ProtocolError::UnknownTriggerSide(other)),
        };
        let mut effect = [0; TRIGGER_EFFECT_LEN];
        effect.copy_from_slice(&body[2..]);

        Ok(Self::TriggerEffect {
            slot: body[0],
            right,
            effect,
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

    fn every_message() -> [ControlMessage; 15] {
        [
            ControlMessage::Hello {
                version: ProtocolVersion::CURRENT,
                formats: FormatSet::ALL,
            },
            ControlMessage::RequestKeyframe { frame: 0xDEAD_BEEF },
            ControlMessage::ReportLoss {
                first_sequence: u32::MAX - 3,
                count: 513,
            },
            ControlMessage::BitrateHint { kbps: 150_000 },
            ControlMessage::Goodbye,
            ControlMessage::Keepalive,
            ControlMessage::Rumble {
                slot: 3,
                strong: 0xFFFF,
                weak: 0x1234,
                delay_ms: 20,
                duration_ms: 0,
            },
            ControlMessage::StreamFormat(crate::stream::tests::hdr_description()),
            ControlMessage::ClockPing {
                sent_micros: 0x0102_0304_0506_0708,
            },
            ControlMessage::ClockPong {
                sent_micros: 1,
                host_micros: u64::MAX,
            },
            ControlMessage::AcknowledgeFrame { frame: u32::MAX },
            ControlMessage::RequestRecovery {
                frame: 0x0102_0304,
                last_good: 0x0102_0300,
            },
            ControlMessage::Lightbar {
                slot: 1,
                red: 255,
                green: 0,
                blue: 128,
            },
            ControlMessage::PlayerLeds {
                slot: 2,
                mask: 0x15,
            },
            ControlMessage::TriggerEffect {
                slot: 3,
                right: true,
                effect: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            },
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
    fn rumble_encoding_is_big_endian_after_the_slot() {
        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];

        let len = ControlMessage::Rumble {
            slot: 2,
            strong: 0x0102,
            weak: 0x0304,
            delay_ms: 0x0506,
            duration_ms: 0x0708,
        }
        .encode(&mut buf)
        .unwrap();

        assert_eq!(&buf[..len], [6, 2, 1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn pad_feedback_messages_have_fixed_layouts() {
        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];

        let lightbar = ControlMessage::Lightbar {
            slot: 1,
            red: 2,
            green: 3,
            blue: 4,
        }
        .encode(&mut buf)
        .unwrap();
        assert_eq!(&buf[..lightbar], [12, 1, 2, 3, 4]);

        let leds = ControlMessage::PlayerLeds { slot: 5, mask: 6 }
            .encode(&mut buf)
            .unwrap();
        assert_eq!(&buf[..leds], [13, 5, 6]);

        let trigger = ControlMessage::TriggerEffect {
            slot: 7,
            right: false,
            effect: [9; TRIGGER_EFFECT_LEN],
        }
        .encode(&mut buf)
        .unwrap();
        assert_eq!(&buf[..trigger], [14, 7, 0, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9]);
    }

    #[test]
    fn a_trigger_side_other_than_left_or_right_is_rejected() {
        let mut bytes = [14, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

        assert_eq!(
            ControlMessage::decode(&bytes),
            Err(ProtocolError::UnknownTriggerSide(2))
        );

        bytes[2] = 1;
        assert!(ControlMessage::decode(&bytes).is_ok());
    }

    #[test]
    fn pad_feedback_messages_with_trailing_or_missing_bytes_are_rejected() {
        let lightbar = [12, 0, 0, 0, 0];
        let leds = [13, 0, 0];

        assert!(ControlMessage::decode(&[lightbar.as_slice(), &[0]].concat()).is_err());
        assert!(ControlMessage::decode(&lightbar[..4]).is_err());
        assert!(ControlMessage::decode(&[leds.as_slice(), &[0]].concat()).is_err());
        assert!(ControlMessage::decode(&leds[..2]).is_err());
        assert!(ControlMessage::decode(&[14, 0, 0]).is_err());
    }

    #[test]
    fn clock_messages_are_big_endian_after_the_tag() {
        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];

        let request_len = ControlMessage::ClockPing {
            sent_micros: 0x0102_0304_0506_0708,
        }
        .encode(&mut buf)
        .unwrap();
        assert_eq!(&buf[..request_len], [8, 1, 2, 3, 4, 5, 6, 7, 8]);

        let reply_len = ControlMessage::ClockPong {
            sent_micros: 1,
            host_micros: 2,
        }
        .encode(&mut buf)
        .unwrap();
        assert_eq!(
            &buf[..reply_len],
            [9, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2]
        );
    }

    #[test]
    fn recovery_messages_are_big_endian_after_the_tag() {
        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];

        let ack_len = ControlMessage::AcknowledgeFrame { frame: 0x0102_0304 }
            .encode(&mut buf)
            .unwrap();
        assert_eq!(&buf[..ack_len], [10, 1, 2, 3, 4]);

        let request_len = ControlMessage::RequestRecovery {
            frame: 0x0506_0708,
            last_good: 0x0102_0304,
        }
        .encode(&mut buf)
        .unwrap();
        assert_eq!(&buf[..request_len], [11, 5, 6, 7, 8, 1, 2, 3, 4]);
    }

    #[test]
    fn trailing_bytes_after_a_recovery_request_are_rejected() {
        assert_eq!(
            ControlMessage::decode(&[TAG_REQUEST_RECOVERY, 0, 0, 0, 1, 0, 0, 0, 0, 0]),
            Err(ProtocolError::TrailingBytes { extra: 1 })
        );
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
    fn hello_carries_the_format_set_after_the_version() {
        let mut buf = [0; ControlMessage::MAX_ENCODED_LEN];

        let len = ControlMessage::Hello {
            version: ProtocolVersion(0x0102),
            formats: FormatSet::from_bits(0x0403_0201).unwrap(),
        }
        .encode(&mut buf)
        .unwrap();

        assert_eq!(&buf[..len], [0, 1, 2, 4, 3, 2, 1]);
    }

    #[test]
    fn hello_with_unknown_format_bits_is_rejected() {
        assert_eq!(
            ControlMessage::decode(&[TAG_HELLO, 0, 14, 0x80, 0, 0, 0]),
            Err(ProtocolError::UnknownFormatBits(0x8000_0000))
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

    #[test]
    fn every_strict_prefix_of_a_control_packet_is_rejected() {
        let mut packets: Vec<_> = every_message()
            .into_iter()
            .map(|message| ControlPacket::Message { id: 7, message })
            .collect();
        packets.push(ControlPacket::Ack { id: 7 });

        for packet in packets {
            let mut buf = [0; ControlPacket::MAX_ENCODED_LEN];
            let len = packet.write(&mut buf).unwrap();

            for cut in 0..len {
                assert!(ControlPacket::read(&buf[..cut]).is_err());
            }
            assert_eq!(ControlPacket::read(&buf[..len]), Ok(packet));
        }
    }
}
