use opus_rs::{Application, OpusDecoder, OpusEncoder, multistream::MultistreamDecoder};
use rusplay_config::AudioConfig;
use rusplay_core::{ChannelLayout, SampleRate};

use crate::{
    AudioDecoder, AudioEncoder, AudioError,
    multistream::{MultistreamEncoder, mapping_table},
};

const OPUS_SAMPLE_RATE: i32 = 48_000;

fn check_sample_rate(rate: SampleRate) -> Result<(), AudioError> {
    match rate {
        SampleRate::Hz48000 => Ok(()),
        SampleRate::Hz96000 => Err(AudioError::OpusSampleRateUnsupported(rate)),
    }
}

fn bitrate_bps(config: &AudioConfig) -> i32 {
    i32::try_from(config.opus.bitrate_kbps.saturating_mul(1000)).unwrap_or(i32::MAX)
}

fn is_multistream(layout: ChannelLayout) -> bool {
    mapping_table(layout).is_some()
}

enum EncoderEngine {
    Single(Box<OpusEncoder>),
    Multistream(MultistreamEncoder),
}

pub struct OpusAudioEncoder {
    engine: EncoderEngine,
    frame_samples: usize,
    channels: usize,
}

impl OpusAudioEncoder {
    pub fn new(config: &AudioConfig) -> Result<Self, AudioError> {
        check_sample_rate(config.sample_rate)?;

        let channels = usize::from(config.channels.channel_count());
        let frame_samples = config.opus.frame_duration.samples_at(config.sample_rate);

        let engine = if is_multistream(config.channels) {
            EncoderEngine::Multistream(MultistreamEncoder::new(
                config.channels,
                frame_samples,
                bitrate_bps(config),
                config.opus.inband_fec,
            )?)
        } else {
            let mut encoder =
                OpusEncoder::new(OPUS_SAMPLE_RATE, channels, Application::RestrictedLowDelay)
                    .map_err(AudioError::OpusEncoderInit)?;
            encoder.bitrate_bps = bitrate_bps(config);
            encoder.use_inband_fec = config.opus.inband_fec;
            EncoderEngine::Single(Box::new(encoder))
        };

        Ok(Self {
            engine,
            frame_samples,
            channels,
        })
    }
}

impl AudioEncoder for OpusAudioEncoder {
    fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize, AudioError> {
        let expected = self.frame_samples * self.channels;
        if pcm.len() != expected {
            return Err(AudioError::FrameLengthMismatch {
                expected,
                found: pcm.len(),
            });
        }

        match &mut self.engine {
            EncoderEngine::Single(encoder) => encoder
                .encode(pcm, self.frame_samples, out)
                .map_err(AudioError::OpusEncode),
            EncoderEngine::Multistream(encoder) => encoder.encode(pcm, out),
        }
    }
}

enum DecoderEngine {
    Single(Box<OpusDecoder>),
    Multistream(MultistreamDecoder),
}

pub struct OpusAudioDecoder {
    engine: DecoderEngine,
    frame_samples: usize,
    channels: usize,
}

impl OpusAudioDecoder {
    pub fn new(config: &AudioConfig) -> Result<Self, AudioError> {
        check_sample_rate(config.sample_rate)?;

        let engine = match mapping_table(config.channels) {
            Some(table) => DecoderEngine::Multistream(
                MultistreamDecoder::new(OPUS_SAMPLE_RATE, table)
                    .map_err(AudioError::OpusDecoderInit)?,
            ),
            None => DecoderEngine::Single(Box::new(
                OpusDecoder::new(
                    OPUS_SAMPLE_RATE,
                    usize::from(config.channels.channel_count()),
                )
                .map_err(AudioError::OpusDecoderInit)?,
            )),
        };

        Ok(Self {
            engine,
            frame_samples: config.opus.frame_duration.samples_at(config.sample_rate),
            channels: usize::from(config.channels.channel_count()),
        })
    }
}

impl AudioDecoder for OpusAudioDecoder {
    fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
        match &mut self.engine {
            DecoderEngine::Single(decoder) => decoder.decode(packet, self.frame_samples, pcm),
            DecoderEngine::Multistream(decoder) => decoder.decode(packet, self.frame_samples, pcm),
        }
        .map_err(AudioError::OpusDecode)
    }

    fn conceal(&mut self, pcm: &mut [f32]) -> Result<usize, AudioError> {
        match &mut self.engine {
            DecoderEngine::Single(decoder) => decoder
                .decode(&[], self.frame_samples, pcm)
                .map_err(AudioError::OpusDecode),
            DecoderEngine::Multistream(_) => {
                let expected = self.frame_samples * self.channels;
                let capacity = pcm.len();
                let silence = pcm
                    .get_mut(..expected)
                    .ok_or(AudioError::FrameLengthMismatch {
                        expected,
                        found: capacity,
                    })?;
                silence.fill(0.0);
                Ok(self.frame_samples)
            }
        }
    }

    fn recover(&mut self, next_packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
        match &mut self.engine {
            DecoderEngine::Single(decoder) => decoder
                .decode_fec(next_packet, self.frame_samples, pcm)
                .map_err(AudioError::OpusDecode),
            DecoderEngine::Multistream(_) => self.conceal(pcm),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{f32::consts::TAU, thread};

    use super::*;
    use crate::CODEC_THREAD_STACK_BYTES;

    const FRAMES_PER_RUN: usize = 40;

    fn on_codec_thread(test: impl FnOnce() + Send + 'static) {
        thread::Builder::new()
            .stack_size(CODEC_THREAD_STACK_BYTES)
            .spawn(test)
            .unwrap()
            .join()
            .unwrap();
    }

    fn config_for(channels: ChannelLayout) -> AudioConfig {
        AudioConfig {
            channels,
            ..AudioConfig::default()
        }
    }

    fn tone_frame(
        frame_index: usize,
        frame_samples: usize,
        channels: usize,
        hot: usize,
    ) -> Vec<f32> {
        let mut input = vec![0.0; frame_samples * channels];
        for frame in 0..frame_samples {
            let position = u16::try_from(frame_index * frame_samples + frame).unwrap_or(0);
            let phase = TAU * 997.0 * f32::from(position) / 48_000.0;
            input[frame * channels + hot] = 0.5 * phase.sin();
        }
        input
    }

    fn channel_energies(layout: ChannelLayout, hot: usize) -> Vec<f32> {
        let config = config_for(layout);
        let channels = usize::from(layout.channel_count());
        let mut encoder = OpusAudioEncoder::new(&config).unwrap();
        let mut decoder = OpusAudioDecoder::new(&config).unwrap();

        let frame_samples = encoder.frame_samples();
        let mut packet = vec![0; 4000];
        let mut output = vec![0.0; frame_samples * channels];
        let mut energies = vec![0.0; channels];

        for frame_index in 0..FRAMES_PER_RUN {
            let input = tone_frame(frame_index, frame_samples, channels, hot);

            let written = encoder.encode(&input, &mut packet).unwrap();
            let produced = decoder.decode(&packet[..written], &mut output).unwrap();
            assert_eq!(produced, frame_samples);

            if frame_index >= FRAMES_PER_RUN / 2 {
                for frame in output.chunks_exact(channels) {
                    for (energy, sample) in energies.iter_mut().zip(frame) {
                        *energy += sample * sample;
                    }
                }
            }
        }

        energies
    }

    fn assert_channel_identity_survives(layout: ChannelLayout) {
        on_codec_thread(move || {
            for hot in 0..usize::from(layout.channel_count()) {
                let energies = channel_energies(layout, hot);

                for (channel, energy) in energies.iter().enumerate() {
                    if channel == hot {
                        assert!(*energy > 1.0, "{layout:?} channel {hot} lost: {energies:?}");
                    } else {
                        assert!(
                            *energy * 20.0 < energies[hot],
                            "{layout:?} channel {hot} leaked into {channel}: {energies:?}"
                        );
                    }
                }
            }
        });
    }

    #[test]
    fn silence_round_trips_through_opus() {
        on_codec_thread(|| {
            let config = AudioConfig::default();
            let mut encoder = OpusAudioEncoder::new(&config).unwrap();
            let mut decoder = OpusAudioDecoder::new(&config).unwrap();

            let samples = encoder.frame_samples() * 2;
            let silence = vec![0.0; samples];
            let mut packet = [0; 1500];
            let mut output = vec![1.0; samples];

            let written = encoder.encode(&silence, &mut packet).unwrap();
            let produced = decoder.decode(&packet[..written], &mut output).unwrap();

            assert!(written > 0);
            assert_eq!(produced, encoder.frame_samples());
        });
    }

    #[test]
    fn surround_51_keeps_every_channel_separate() {
        assert_channel_identity_survives(ChannelLayout::Surround51);
    }

    #[test]
    fn surround_71_keeps_every_channel_separate() {
        assert_channel_identity_survives(ChannelLayout::Surround71);
    }

    #[test]
    fn a_lost_stereo_frame_is_concealed_with_audio() {
        on_codec_thread(|| {
            let config = config_for(ChannelLayout::Stereo);
            let mut encoder = OpusAudioEncoder::new(&config).unwrap();
            let mut decoder = OpusAudioDecoder::new(&config).unwrap();

            let frame_samples = encoder.frame_samples();
            let mut packet = vec![0; 4000];
            let mut output = vec![0.0; frame_samples * 2];

            for frame_index in 0..FRAMES_PER_RUN {
                let input = tone_frame(frame_index, frame_samples, 2, 0);
                let written = encoder.encode(&input, &mut packet).unwrap();
                decoder.decode(&packet[..written], &mut output).unwrap();
            }

            let produced = decoder.conceal(&mut output).unwrap();
            let energy: f32 = output.iter().map(|sample| sample * sample).sum();

            assert_eq!(produced, frame_samples);
            assert!(energy > 0.0);
        });
    }

    #[test]
    fn a_lost_frame_is_recovered_from_the_next_packet() {
        on_codec_thread(|| {
            let config = config_for(ChannelLayout::Stereo);
            let mut encoder = OpusAudioEncoder::new(&config).unwrap();
            let mut decoder = OpusAudioDecoder::new(&config).unwrap();

            let frame_samples = encoder.frame_samples();
            let mut packet = vec![0; 4000];
            let mut output = vec![0.0; frame_samples * 2];

            for frame_index in 0..FRAMES_PER_RUN {
                let input = tone_frame(frame_index, frame_samples, 2, 0);
                let written = encoder.encode(&input, &mut packet).unwrap();
                decoder.decode(&packet[..written], &mut output).unwrap();
            }

            let input = tone_frame(FRAMES_PER_RUN, frame_samples, 2, 0);
            let written = encoder.encode(&input, &mut packet).unwrap();
            let produced = decoder.recover(&packet[..written], &mut output).unwrap();

            assert_eq!(produced, frame_samples);
        });
    }

    #[test]
    fn a_lost_surround_frame_is_silent() {
        on_codec_thread(|| {
            let config = config_for(ChannelLayout::Surround51);
            let mut decoder = OpusAudioDecoder::new(&config).unwrap();

            let mut output = vec![1.0; decoder.frame_samples * 6];
            let produced = decoder.conceal(&mut output).unwrap();

            assert_eq!(produced, decoder.frame_samples);
            assert!(output.iter().all(|sample| sample.to_bits() == 0));
        });
    }

    #[test]
    fn surround_rejects_wrong_frame_length() {
        on_codec_thread(|| {
            let config = config_for(ChannelLayout::Surround51);
            let mut encoder = OpusAudioEncoder::new(&config).unwrap();
            let mut packet = [0; 1500];

            let wrong = vec![0.0; encoder.frame_samples() * 2];

            assert!(matches!(
                encoder.encode(&wrong, &mut packet),
                Err(AudioError::FrameLengthMismatch { .. })
            ));
        });
    }

    #[test]
    fn short_packet_buffer_is_reported_for_surround() {
        on_codec_thread(|| {
            let config = config_for(ChannelLayout::Surround51);
            let mut encoder = OpusAudioEncoder::new(&config).unwrap();
            let mut packet = [0; 2];

            let silence = vec![0.0; encoder.frame_samples() * 6];

            assert!(matches!(
                encoder.encode(&silence, &mut packet),
                Err(AudioError::OpusPacketBufferTooSmall { capacity: 2, .. })
            ));
        });
    }

    #[test]
    fn stereo_and_mono_stay_single_stream() {
        assert!(!is_multistream(ChannelLayout::Mono));
        assert!(!is_multistream(ChannelLayout::Stereo));
    }
}
