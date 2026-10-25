use std::{io::Cursor, mem};

use claxon::frame::FrameReader;
use flacenc::{
    bitsink::ByteSink,
    component::{BitRepr, StreamInfo},
    config::{self, OrderSel},
    encode_fixed_size_frame,
    error::{EncodeError, Verified, Verify},
    source::{Fill, FrameBuf},
};
use rusplay_config::AudioConfig;

use crate::{AudioDecoder, AudioEncoder, AudioError};

const BITS_PER_SAMPLE: usize = 24;
const FULL_SCALE: f32 = 8_388_608.0;
const LARGEST_SAMPLE: f32 = FULL_SCALE - 1.0;
const FIRST_LPC_LEVEL: u8 = 3;
const LPC_ORDERS: [usize; 6] = [4, 6, 8, 10, 12, 12];
const BIT_COUNT_ORDER_LEVEL: u8 = 8;
const FRAME_NUMBER_MASK: usize = 0x7fff_ffff;

#[allow(clippy::cast_possible_truncation)]
fn quantise(sample: f32) -> i32 {
    (sample * FULL_SCALE)
        .round()
        .clamp(-FULL_SCALE, LARGEST_SAMPLE) as i32
}

#[allow(clippy::cast_precision_loss)]
fn dequantise(sample: i32) -> f32 {
    sample as f32 / FULL_SCALE
}

fn encode_error(error: EncodeError) -> AudioError {
    match error {
        EncodeError::Config(source) => AudioError::FlacSettings(source),
        _ => AudioError::FlacSampleIntake,
    }
}

fn encoder_settings(
    compression_level: u8,
    block_size: usize,
) -> Result<Verified<config::Encoder>, AudioError> {
    let mut settings = config::Encoder::default();
    settings.block_size = block_size;
    settings.multithread = false;

    settings.stereo_coding.use_leftside = compression_level >= 2;
    settings.stereo_coding.use_rightside = compression_level >= 2;
    settings.stereo_coding.use_midside = compression_level >= 1;

    settings.subframe_coding.use_lpc = compression_level >= FIRST_LPC_LEVEL;
    let order_index = usize::from(compression_level.saturating_sub(FIRST_LPC_LEVEL));
    if let Some(order) = LPC_ORDERS.get(order_index) {
        settings.subframe_coding.qlpc.lpc_order = *order;
    }
    if compression_level >= BIT_COUNT_ORDER_LEVEL {
        settings.subframe_coding.fixed.order_sel = OrderSel::BitCount;
    }

    settings
        .into_verified()
        .map_err(|(_, source)| AudioError::FlacSettings(source))
}

pub struct FlacAudioEncoder {
    settings: Verified<config::Encoder>,
    stream_info: StreamInfo,
    framebuf: FrameBuf,
    sink: ByteSink,
    quantised: Vec<i32>,
    block_size: usize,
    channels: usize,
    frame_number: usize,
}

impl FlacAudioEncoder {
    pub fn new(config: &AudioConfig) -> Result<Self, AudioError> {
        let channels = usize::from(config.channels.channel_count());
        let block_size = usize::from(config.flac.block_size);
        let sample_rate = config.sample_rate.hz() as usize;

        let settings = encoder_settings(config.flac.compression_level, block_size)?;
        let stream_info = StreamInfo::new(sample_rate, channels, BITS_PER_SAMPLE)
            .map_err(AudioError::FlacSettings)?;
        let framebuf =
            FrameBuf::with_size(channels, block_size).map_err(AudioError::FlacSettings)?;

        Ok(Self {
            settings,
            stream_info,
            framebuf,
            sink: ByteSink::new(),
            quantised: vec![0; block_size * channels],
            block_size,
            channels,
            frame_number: 0,
        })
    }
}

impl AudioEncoder for FlacAudioEncoder {
    fn frame_samples(&self) -> usize {
        self.block_size
    }

    fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize, AudioError> {
        let expected = self.block_size * self.channels;
        if pcm.len() != expected {
            return Err(AudioError::FrameLengthMismatch {
                expected,
                found: pcm.len(),
            });
        }

        for (slot, sample) in self.quantised.iter_mut().zip(pcm) {
            *slot = quantise(*sample);
        }
        self.framebuf
            .fill_interleaved(&self.quantised)
            .map_err(|_| AudioError::FlacSampleIntake)?;

        let frame = encode_fixed_size_frame(
            &self.settings,
            &self.framebuf,
            self.frame_number,
            &self.stream_info,
        )
        .map_err(encode_error)?;
        self.frame_number = (self.frame_number + 1) & FRAME_NUMBER_MASK;

        self.sink.clear();
        frame.write(&mut self.sink).map_err(AudioError::FlacWrite)?;

        let bytes = self.sink.as_slice();
        let capacity = out.len();
        let destination =
            out.get_mut(..bytes.len())
                .ok_or(AudioError::FlacPacketBufferTooSmall {
                    needed: bytes.len(),
                    capacity,
                })?;
        destination.copy_from_slice(bytes);

        Ok(bytes.len())
    }
}

pub struct FlacAudioDecoder {
    channels: usize,
    buffer: Vec<i32>,
}

impl FlacAudioDecoder {
    pub fn new(config: &AudioConfig) -> Result<Self, AudioError> {
        Ok(Self {
            channels: usize::from(config.channels.channel_count()),
            buffer: Vec::new(),
        })
    }
}

impl AudioDecoder for FlacAudioDecoder {
    fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
        let mut reader = FrameReader::new(Cursor::new(packet));
        let block = reader
            .read_next_or_eof(mem::take(&mut self.buffer))
            .map_err(AudioError::FlacDecode)?
            .ok_or(AudioError::FlacEmptyPacket)?;

        let found_channels = block.channels() as usize;
        if found_channels != self.channels {
            return Err(AudioError::FlacChannelMismatch {
                expected: self.channels,
                found: found_channels,
            });
        }

        let frames = block.duration() as usize;
        let expected = frames * self.channels;
        let capacity = pcm.len();
        let destination = pcm
            .get_mut(..expected)
            .ok_or(AudioError::FrameLengthMismatch {
                expected,
                found: capacity,
            })?;

        for channel in 0..self.channels {
            let samples = block.channel(u32::try_from(channel).unwrap_or(u32::MAX));
            for (frame, sample) in samples.iter().enumerate() {
                destination[frame * self.channels + channel] = dequantise(*sample);
            }
        }

        self.buffer = block.into_buffer();

        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::ChannelLayout;

    use super::*;

    fn config(layout: ChannelLayout, compression_level: u8) -> AudioConfig {
        let mut config = AudioConfig {
            channels: layout,
            ..AudioConfig::default()
        };
        config.flac.compression_level = compression_level;
        config
    }

    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let signed = i32::from_ne_bytes(state.to_ne_bytes()) >> 8;
                dequantise(signed)
            })
            .collect()
    }

    fn bits(samples: &[f32]) -> Vec<u32> {
        samples.iter().map(|sample| sample.to_bits()).collect()
    }

    fn round_trip(config: &AudioConfig) {
        let mut encoder = FlacAudioEncoder::new(config).unwrap();
        let mut decoder = FlacAudioDecoder::new(config).unwrap();
        let channels = usize::from(config.channels.channel_count());

        let mut packet = vec![0; 16 * 1024];
        let mut output = vec![0.0; encoder.frame_samples() * channels];

        for seed in 0..4 {
            let input = noise(encoder.frame_samples() * channels, seed);

            let written = encoder.encode(&input, &mut packet).unwrap();
            let produced = decoder.decode(&packet[..written], &mut output).unwrap();

            assert_eq!(produced, encoder.frame_samples());
            assert_eq!(bits(&input), bits(&output));
        }
    }

    #[test]
    fn stereo_round_trip_is_bit_exact_at_every_level() {
        for level in 0..=8 {
            round_trip(&config(ChannelLayout::Stereo, level));
        }
    }

    #[test]
    fn every_layout_round_trips_bit_exactly() {
        for layout in [
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            ChannelLayout::Surround51,
            ChannelLayout::Surround71,
        ] {
            round_trip(&config(layout, 5));
        }
    }

    #[test]
    fn silence_and_full_scale_survive() {
        let config = AudioConfig::default();
        let mut encoder = FlacAudioEncoder::new(&config).unwrap();
        let mut decoder = FlacAudioDecoder::new(&config).unwrap();

        let samples = encoder.frame_samples() * 2;
        let mut packet = vec![0; 16 * 1024];
        let mut output = vec![0.5; samples];

        for level in [0.0, 1.0, -1.0] {
            let input = vec![level; samples];
            let written = encoder.encode(&input, &mut packet).unwrap();
            decoder.decode(&packet[..written], &mut output).unwrap();

            let expected = dequantise(quantise(level));
            assert!(
                bits(&output)
                    .iter()
                    .all(|found| *found == expected.to_bits())
            );
        }
    }

    #[test]
    fn short_output_buffer_is_reported() {
        let config = AudioConfig::default();
        let mut encoder = FlacAudioEncoder::new(&config).unwrap();

        let input = vec![0.25; encoder.frame_samples() * 2];
        let mut packet = [0; 4];

        assert!(matches!(
            encoder.encode(&input, &mut packet),
            Err(AudioError::FlacPacketBufferTooSmall { capacity: 4, .. })
        ));
    }

    #[test]
    fn truncated_packet_is_an_error() {
        let config = AudioConfig::default();
        let mut encoder = FlacAudioEncoder::new(&config).unwrap();
        let mut decoder = FlacAudioDecoder::new(&config).unwrap();

        let input = noise(encoder.frame_samples() * 2, 7);
        let mut packet = vec![0; 16 * 1024];
        let mut output = vec![0.0; input.len()];

        let written = encoder.encode(&input, &mut packet).unwrap();

        assert!(decoder.decode(&packet[..written / 2], &mut output).is_err());
        assert!(matches!(
            decoder.decode(&[], &mut output),
            Err(AudioError::FlacEmptyPacket)
        ));
    }

    #[test]
    fn unsupported_block_size_is_rejected() {
        let mut config = AudioConfig::default();
        config.flac.block_size = 8;

        assert!(matches!(
            FlacAudioEncoder::new(&config),
            Err(AudioError::FlacSettings(_))
        ));
    }
}
