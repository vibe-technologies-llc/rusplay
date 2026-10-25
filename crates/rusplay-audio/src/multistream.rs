use opus_rs::{Application, OpusEncoder, multistream::ChannelMappingTable};
use rusplay_core::ChannelLayout;

use crate::AudioError;

const SAMPLE_RATE: i32 = 48_000;
const MAX_SUB_PACKET_BYTES: usize = 1500;
const TWO_BYTE_LENGTH_FLOOR: usize = 252;
const CODE_THREE_VBR: u8 = 0x80;
const CODE_THREE_PADDING: u8 = 0x40;
const CODE_THREE_FRAME_COUNT: u8 = 0x3f;
const PADDING_CONTINUATION: u8 = 255;
const PADDING_CONTINUATION_BYTES: usize = 254;

struct Shape {
    coupled: u8,
    mono: u8,
    mapping: &'static [u8],
}

fn shape(layout: ChannelLayout) -> Option<Shape> {
    match layout {
        ChannelLayout::Mono | ChannelLayout::Stereo => None,
        ChannelLayout::Surround51 => Some(Shape {
            coupled: 2,
            mono: 2,
            mapping: &[0, 1, 4, 5, 2, 3],
        }),
        ChannelLayout::Surround71 => Some(Shape {
            coupled: 3,
            mono: 2,
            mapping: &[0, 1, 6, 7, 4, 5, 2, 3],
        }),
    }
}

pub(crate) fn mapping_table(layout: ChannelLayout) -> Option<ChannelMappingTable> {
    shape(layout).map(|shape| ChannelMappingTable {
        channels: layout.channel_count(),
        stream_count: shape.coupled + shape.mono,
        coupled_count: shape.coupled,
        mapping: shape.mapping.to_vec(),
    })
}

fn push_length(length: usize, out: &mut Vec<u8>) -> Option<()> {
    if length < TWO_BYTE_LENGTH_FLOOR {
        out.push(u8::try_from(length).ok()?);
        return Some(());
    }

    let first = TWO_BYTE_LENGTH_FLOOR + (length - TWO_BYTE_LENGTH_FLOOR) % 4;
    out.push(u8::try_from(first).ok()?);
    out.push(u8::try_from((length - first) / 4).ok()?);
    Some(())
}

fn read_length(data: &[u8]) -> Option<(usize, usize)> {
    let first = usize::from(*data.first()?);
    if first < TWO_BYTE_LENGTH_FLOOR {
        return Some((first, 1));
    }

    let second = usize::from(*data.get(1)?);
    Some((second * 4 + first, 2))
}

fn self_delimit(packet: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let toc = *packet.first()?;
    out.push(toc);

    match toc & 0x03 {
        0 => {
            push_length(packet.len() - 1, out)?;
            out.extend_from_slice(&packet[1..]);
        }
        1 => {
            let frames = &packet[1..];
            if !frames.len().is_multiple_of(2) {
                return None;
            }
            push_length(frames.len() / 2, out)?;
            out.extend_from_slice(frames);
        }
        2 => {
            let (first, length_bytes) = read_length(&packet[1..])?;
            let frames = &packet[1 + length_bytes..];
            let second = frames.len().checked_sub(first)?;
            out.extend_from_slice(&packet[1..=length_bytes]);
            push_length(second, out)?;
            out.extend_from_slice(frames);
        }
        _ => self_delimit_counted(packet, out)?,
    }

    Some(())
}

fn self_delimit_counted(packet: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let flags = *packet.get(1)?;
    let frame_count = usize::from(flags & CODE_THREE_FRAME_COUNT);
    if frame_count == 0 {
        return None;
    }
    out.push(flags);

    let mut cursor = 2;
    let mut padding = 0;
    if flags & CODE_THREE_PADDING != 0 {
        loop {
            let byte = *packet.get(cursor)?;
            out.push(byte);
            cursor += 1;
            if byte == PADDING_CONTINUATION {
                padding += PADDING_CONTINUATION_BYTES;
            } else {
                padding += usize::from(byte);
                break;
            }
        }
    }

    let mut explicit_total = 0;
    if flags & CODE_THREE_VBR != 0 {
        for _ in 1..frame_count {
            let (length, length_bytes) = read_length(packet.get(cursor..)?)?;
            out.extend_from_slice(&packet[cursor..cursor + length_bytes]);
            cursor += length_bytes;
            explicit_total += length;
        }
        let remaining = packet
            .len()
            .checked_sub(cursor + padding + explicit_total)?;
        push_length(remaining, out)?;
    } else {
        let frames = packet.len().checked_sub(cursor + padding)?;
        if !frames.is_multiple_of(frame_count) {
            return None;
        }
        push_length(frames / frame_count, out)?;
    }

    out.extend_from_slice(&packet[cursor..]);
    Some(())
}

pub(crate) struct MultistreamEncoder {
    encoders: Vec<OpusEncoder>,
    stream_channels: Vec<usize>,
    sources: Vec<usize>,
    channels: usize,
    frame_samples: usize,
    stream_input: Vec<f32>,
    stream_packet: Vec<u8>,
    assembled: Vec<u8>,
}

impl MultistreamEncoder {
    pub(crate) fn new(
        layout: ChannelLayout,
        frame_samples: usize,
        bitrate_bps: i32,
        inband_fec: bool,
    ) -> Result<Self, AudioError> {
        let table = mapping_table(layout).ok_or(AudioError::OpusNotMultistream(layout))?;
        let channels = usize::from(table.channels);

        let stream_channels: Vec<usize> = (0..table.stream_count)
            .map(|stream| if stream < table.coupled_count { 2 } else { 1 })
            .collect();
        let bitrate_per_channel = bitrate_bps / i32::from(table.channels);

        let mut encoders = Vec::with_capacity(stream_channels.len());
        for &stream_channel_count in &stream_channels {
            let mut encoder = OpusEncoder::new(
                SAMPLE_RATE,
                stream_channel_count,
                Application::RestrictedLowDelay,
            )
            .map_err(AudioError::OpusEncoderInit)?;
            encoder.bitrate_bps = bitrate_per_channel
                .saturating_mul(i32::try_from(stream_channel_count).unwrap_or(i32::MAX));
            encoder.use_inband_fec = inband_fec;
            encoders.push(encoder);
        }

        let mut sources = vec![0; channels];
        for (output_channel, &decoded) in table.mapping.iter().enumerate() {
            sources[usize::from(decoded)] = output_channel;
        }

        Ok(Self {
            encoders,
            stream_channels,
            sources,
            channels,
            frame_samples,
            stream_input: Vec::new(),
            stream_packet: vec![0; MAX_SUB_PACKET_BYTES],
            assembled: Vec::new(),
        })
    }

    pub(crate) fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize, AudioError> {
        self.assembled.clear();

        let last = self.encoders.len() - 1;
        let mut decoded_offset = 0;
        for (stream, encoder) in self.encoders.iter_mut().enumerate() {
            let stream_channels = self.stream_channels[stream];
            let sources = &self.sources[decoded_offset..decoded_offset + stream_channels];

            self.stream_input.clear();
            for frame in pcm.chunks_exact(self.channels) {
                self.stream_input
                    .extend(sources.iter().map(|&source| frame[source]));
            }

            let written = encoder
                .encode(
                    &self.stream_input,
                    self.frame_samples,
                    &mut self.stream_packet,
                )
                .map_err(AudioError::OpusEncode)?;
            let packet = &self.stream_packet[..written];

            if stream == last {
                self.assembled.extend_from_slice(packet);
            } else {
                self_delimit(packet, &mut self.assembled)
                    .ok_or(AudioError::OpusSubPacketMalformed { stream })?;
            }
            decoded_offset += stream_channels;
        }

        let capacity = out.len();
        let destination =
            out.get_mut(..self.assembled.len())
                .ok_or(AudioError::OpusPacketBufferTooSmall {
                    needed: self.assembled.len(),
                    capacity,
                })?;
        destination.copy_from_slice(&self.assembled);

        Ok(self.assembled.len())
    }
}

#[cfg(test)]
mod tests {
    use opus_rs::multistream::split_self_delimited;

    use super::*;

    fn delimited_then_split(packet: &[u8]) -> Vec<u8> {
        let mut delimited = Vec::new();
        self_delimit(packet, &mut delimited).unwrap();

        let mut scratch = Vec::new();
        let (restored, consumed) = split_self_delimited(&delimited, &mut scratch).unwrap();

        assert_eq!(consumed, delimited.len());
        restored.to_vec()
    }

    fn packet_with(toc: u8, body: &[u8]) -> Vec<u8> {
        let mut packet = vec![toc];
        packet.extend_from_slice(body);
        packet
    }

    #[test]
    fn code_zero_survives_framing() {
        let packet = packet_with(0, &[1, 2, 3, 4, 5]);

        assert_eq!(delimited_then_split(&packet), packet);
    }

    #[test]
    fn long_code_zero_uses_two_byte_length() {
        for length in [251, 252, 253, 254, 255, 256, 1000, 1275] {
            let packet = packet_with(0, &vec![7; length]);

            assert_eq!(delimited_then_split(&packet), packet);
        }
    }

    #[test]
    fn code_one_survives_framing() {
        let packet = packet_with(1, &[1, 2, 3, 4]);

        assert_eq!(delimited_then_split(&packet), packet);
    }

    #[test]
    fn code_two_survives_framing() {
        let packet = packet_with(2, &[3, 0xa, 0xb, 0xc, 0xd, 0xe]);

        assert_eq!(delimited_then_split(&packet), packet);
    }

    #[test]
    fn code_three_cbr_survives_framing() {
        let packet = packet_with(3, &[3, 1, 2, 3, 4, 5, 6]);

        assert_eq!(delimited_then_split(&packet), packet);
    }

    #[test]
    fn code_three_vbr_survives_framing() {
        let packet = packet_with(3, &[0x80 | 3, 2, 3, 1, 2, 3, 4, 5, 6]);

        assert_eq!(delimited_then_split(&packet), packet);
    }

    #[test]
    fn code_three_with_padding_survives_framing() {
        let packet = packet_with(3, &[0x40 | 2, 2, 1, 2, 3, 4, 9, 9]);

        assert_eq!(delimited_then_split(&packet), packet);
    }

    #[test]
    fn malformed_packets_are_refused() {
        let mut out = Vec::new();

        assert!(self_delimit(&[], &mut out).is_none());
        assert!(self_delimit(&[1, 1, 2, 3], &mut out).is_none());
        assert!(self_delimit(&[3, 0], &mut out).is_none());
    }

    #[test]
    fn surround_tables_cover_every_channel_once() {
        for layout in [ChannelLayout::Surround51, ChannelLayout::Surround71] {
            let table = mapping_table(layout).unwrap();
            let mut seen = table.mapping.clone();
            seen.sort_unstable();

            let expected: Vec<u8> = (0..table.channels).collect();
            assert_eq!(seen, expected);
        }
    }

    #[test]
    fn plain_layouts_have_no_table() {
        assert!(mapping_table(ChannelLayout::Mono).is_none());
        assert!(mapping_table(ChannelLayout::Stereo).is_none());
    }
}
