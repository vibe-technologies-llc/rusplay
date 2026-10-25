use opus_rs::{Application, OpusDecoder, OpusEncoder};
use rusplay_config::AudioConfig;
use rusplay_core::{ChannelLayout, SampleRate};

use crate::{AudioDecoder, AudioEncoder, AudioError};

const OPUS_SAMPLE_RATE: i32 = 48_000;

fn opus_channels(layout: ChannelLayout) -> Result<usize, AudioError> {
    match layout {
        ChannelLayout::Mono | ChannelLayout::Stereo => Ok(usize::from(layout.channel_count())),
        ChannelLayout::Surround51 | ChannelLayout::Surround71 => {
            Err(AudioError::OpusLayoutUnsupported(layout))
        }
    }
}

fn check_sample_rate(rate: SampleRate) -> Result<(), AudioError> {
    match rate {
        SampleRate::Hz48000 => Ok(()),
        SampleRate::Hz96000 => Err(AudioError::OpusSampleRateUnsupported(rate)),
    }
}

pub struct OpusAudioEncoder {
    encoder: OpusEncoder,
    frame_samples: usize,
    channels: usize,
}

impl OpusAudioEncoder {
    pub fn new(config: &AudioConfig) -> Result<Self, AudioError> {
        check_sample_rate(config.sample_rate)?;
        let channels = opus_channels(config.channels)?;

        let mut encoder =
            OpusEncoder::new(OPUS_SAMPLE_RATE, channels, Application::RestrictedLowDelay)
                .map_err(AudioError::OpusEncoderInit)?;
        encoder.bitrate_bps =
            i32::try_from(config.opus.bitrate_kbps.saturating_mul(1000)).unwrap_or(i32::MAX);
        encoder.use_inband_fec = config.opus.inband_fec;

        Ok(Self {
            encoder,
            frame_samples: config.opus.frame_duration.samples_at(config.sample_rate),
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

        self.encoder
            .encode(pcm, self.frame_samples, out)
            .map_err(AudioError::OpusEncode)
    }
}

pub struct OpusAudioDecoder {
    decoder: OpusDecoder,
    frame_samples: usize,
}

impl OpusAudioDecoder {
    pub fn new(config: &AudioConfig) -> Result<Self, AudioError> {
        check_sample_rate(config.sample_rate)?;
        let channels = opus_channels(config.channels)?;

        let decoder =
            OpusDecoder::new(OPUS_SAMPLE_RATE, channels).map_err(AudioError::OpusDecoderInit)?;

        Ok(Self {
            decoder,
            frame_samples: config.opus.frame_duration.samples_at(config.sample_rate),
        })
    }
}

impl AudioDecoder for OpusAudioDecoder {
    fn decode(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, AudioError> {
        self.decoder
            .decode(packet, self.frame_samples, pcm)
            .map_err(AudioError::OpusDecode)
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;
    use crate::CODEC_THREAD_STACK_BYTES;

    fn on_codec_thread(test: impl FnOnce() + Send + 'static) {
        thread::Builder::new()
            .stack_size(CODEC_THREAD_STACK_BYTES)
            .spawn(test)
            .unwrap()
            .join()
            .unwrap();
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
    fn surround_is_rejected_until_multistream_lands() {
        let config = AudioConfig {
            channels: ChannelLayout::Surround51,
            ..AudioConfig::default()
        };

        assert!(matches!(
            OpusAudioEncoder::new(&config),
            Err(AudioError::OpusLayoutUnsupported(ChannelLayout::Surround51))
        ));
    }
}
