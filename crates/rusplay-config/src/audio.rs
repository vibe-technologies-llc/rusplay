use rusplay_core::{AudioCodec, ChannelLayout, SampleRate};
use serde::{Deserialize, Serialize};

use crate::ConfigError;

const MAX_FLAC_COMPRESSION: u8 = 8;
const MAX_JITTER_BUFFER_MS: u32 = 500;
const MAX_FEC_OVERHEAD_PERCENT: u8 = 100;
const MICROPHONE_CHANNELS: ChannelLayout = ChannelLayout::Mono;
const FLAC_BLOCK_SIZES: std::ops::RangeInclusive<u16> = 32..=32767;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub codec: AudioCodec,
    pub sample_rate: SampleRate,
    pub channels: ChannelLayout,
    pub jitter_buffer_ms: u32,
    pub decode_queue_packets: u16,
    pub max_conceal_ms: u16,
    pub fec_overhead_percent: u8,
    pub opus: OpusConfig,
    pub flac: FlacConfig,
    pub microphone: MicrophoneConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            codec: AudioCodec::default(),
            sample_rate: SampleRate::default(),
            channels: ChannelLayout::default(),
            jitter_buffer_ms: 10,
            decode_queue_packets: 32,
            max_conceal_ms: 50,
            fec_overhead_percent: 0,
            opus: OpusConfig::default(),
            flac: FlacConfig::default(),
            microphone: MicrophoneConfig::default(),
        }
    }
}

impl AudioConfig {
    pub const fn frame_samples(&self) -> usize {
        match self.codec {
            AudioCodec::Opus => self.opus.frame_duration.samples_at(self.sample_rate),
            AudioCodec::Flac => self.flac.block_size as usize,
        }
    }

    #[must_use]
    pub fn microphone_stream(&self) -> Self {
        let microphone = &self.microphone;

        Self {
            codec: AudioCodec::Opus,
            channels: MICROPHONE_CHANNELS,
            jitter_buffer_ms: microphone.jitter_buffer_ms,
            fec_overhead_percent: microphone.fec_overhead_percent,
            opus: OpusConfig {
                bitrate_kbps: microphone.bitrate_kbps,
                frame_duration: microphone.frame_duration,
                inband_fec: microphone.inband_fec,
            },
            ..*self
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.opus.bitrate_kbps == 0 {
            return Err(ConfigError::ZeroAudioBitrate);
        }

        if self.flac.compression_level > MAX_FLAC_COMPRESSION {
            return Err(ConfigError::FlacCompressionOutOfRange(
                self.flac.compression_level,
            ));
        }

        if self.decode_queue_packets == 0 {
            return Err(ConfigError::ZeroAudioDecodeQueue);
        }

        if self.jitter_buffer_ms > MAX_JITTER_BUFFER_MS {
            return Err(ConfigError::JitterBufferOutOfRange(self.jitter_buffer_ms));
        }

        if !FLAC_BLOCK_SIZES.contains(&self.flac.block_size) {
            return Err(ConfigError::FlacBlockSizeOutOfRange(self.flac.block_size));
        }

        if self.fec_overhead_percent > MAX_FEC_OVERHEAD_PERCENT {
            return Err(ConfigError::AudioFecOverheadOutOfRange(
                self.fec_overhead_percent,
            ));
        }

        self.microphone.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MicrophoneConfig {
    pub enabled: bool,
    pub bitrate_kbps: u32,
    pub frame_duration: OpusFrameDuration,
    pub inband_fec: bool,
    pub jitter_buffer_ms: u32,
    pub fec_overhead_percent: u8,
}

impl Default for MicrophoneConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bitrate_kbps: 32,
            frame_duration: OpusFrameDuration::Ms10,
            inband_fec: true,
            jitter_buffer_ms: 20,
            fec_overhead_percent: 0,
        }
    }
}

impl MicrophoneConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.bitrate_kbps == 0 {
            return Err(ConfigError::ZeroMicrophoneBitrate);
        }

        if self.jitter_buffer_ms > MAX_JITTER_BUFFER_MS {
            return Err(ConfigError::MicrophoneJitterBufferOutOfRange(
                self.jitter_buffer_ms,
            ));
        }

        if self.fec_overhead_percent > MAX_FEC_OVERHEAD_PERCENT {
            return Err(ConfigError::MicrophoneFecOverheadOutOfRange(
                self.fec_overhead_percent,
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OpusFrameDuration {
    #[serde(rename = "2.5ms")]
    Ms2_5,
    #[default]
    #[serde(rename = "5ms")]
    Ms5,
    #[serde(rename = "10ms")]
    Ms10,
    #[serde(rename = "20ms")]
    Ms20,
}

impl OpusFrameDuration {
    pub const fn samples_at(self, rate: SampleRate) -> usize {
        let hz = rate.hz() as usize;
        match self {
            Self::Ms2_5 => hz / 400,
            Self::Ms5 => hz / 200,
            Self::Ms10 => hz / 100,
            Self::Ms20 => hz / 50,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpusConfig {
    pub bitrate_kbps: u32,
    pub frame_duration: OpusFrameDuration,
    pub inband_fec: bool,
}

impl Default for OpusConfig {
    fn default() -> Self {
        Self {
            bitrate_kbps: 256,
            frame_duration: OpusFrameDuration::default(),
            inband_fec: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FlacConfig {
    pub compression_level: u8,
    pub block_size: u16,
}

impl Default for FlacConfig {
    fn default() -> Self {
        Self {
            compression_level: 0,
            block_size: 240,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_buffer_defaults_to_ten_milliseconds() {
        assert_eq!(AudioConfig::default().jitter_buffer_ms, 10);
    }

    #[test]
    fn client_queue_and_concealment_defaults() {
        let config = AudioConfig::default();

        assert_eq!(config.decode_queue_packets, 32);
        assert_eq!(config.max_conceal_ms, 50);
    }

    #[test]
    fn empty_decode_queue_is_rejected() {
        let config = AudioConfig {
            decode_queue_packets: 0,
            ..AudioConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::ZeroAudioDecodeQueue)
        ));
    }

    #[test]
    fn queue_and_concealment_parse_from_toml() {
        let parsed: AudioConfig =
            toml::from_str("decode_queue_packets = 8\nmax_conceal_ms = 0").unwrap();

        assert_eq!(parsed.decode_queue_packets, 8);
        assert_eq!(parsed.max_conceal_ms, 0);
    }

    #[test]
    fn jitter_buffer_above_the_ceiling_is_rejected() {
        let config = AudioConfig {
            jitter_buffer_ms: 501,
            ..AudioConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::JitterBufferOutOfRange(501))
        ));
    }

    #[test]
    fn jitter_buffer_parses_from_toml() {
        let parsed: AudioConfig = toml::from_str("jitter_buffer_ms = 25").unwrap();

        assert_eq!(parsed.jitter_buffer_ms, 25);
    }

    #[test]
    fn flac_block_size_outside_the_format_is_rejected() {
        for block_size in [0, 31, 32768] {
            let config = AudioConfig {
                flac: FlacConfig {
                    block_size,
                    ..FlacConfig::default()
                },
                ..AudioConfig::default()
            };

            assert!(matches!(
                config.validate(),
                Err(ConfigError::FlacBlockSizeOutOfRange(found)) if found == block_size
            ));
        }
    }

    #[test]
    fn audio_fec_is_off_by_default_and_capped_at_one_hundred_percent() {
        let excessive = AudioConfig {
            fec_overhead_percent: 101,
            ..AudioConfig::default()
        };

        assert_eq!(AudioConfig::default().fec_overhead_percent, 0);
        assert!(matches!(
            excessive.validate(),
            Err(ConfigError::AudioFecOverheadOutOfRange(101))
        ));
    }

    #[test]
    fn the_microphone_is_off_by_default_and_speaks_mono_voice_opus() {
        let config = AudioConfig::default();
        let stream = config.microphone_stream();

        assert!(!config.microphone.enabled);
        assert_eq!(stream.codec, AudioCodec::Opus);
        assert_eq!(stream.channels, ChannelLayout::Mono);
        assert_eq!(stream.opus.bitrate_kbps, config.microphone.bitrate_kbps);
        assert_eq!(stream.opus.frame_duration, OpusFrameDuration::Ms10);
        assert_eq!(stream.jitter_buffer_ms, config.microphone.jitter_buffer_ms);
        assert_eq!(stream.sample_rate, config.sample_rate);
        assert_eq!(stream.decode_queue_packets, config.decode_queue_packets);
    }

    #[test]
    fn the_microphone_stream_inherits_the_main_audio_loss_policy() {
        let config = AudioConfig {
            max_conceal_ms: 80,
            decode_queue_packets: 7,
            ..AudioConfig::default()
        };

        let stream = config.microphone_stream();

        assert_eq!(stream.max_conceal_ms, 80);
        assert_eq!(stream.decode_queue_packets, 7);
    }

    #[test]
    fn frame_samples_follow_the_codec() {
        let opus = AudioConfig::default();
        let flac = AudioConfig {
            codec: AudioCodec::Flac,
            ..AudioConfig::default()
        };

        assert_eq!(opus.frame_samples(), 240);
        assert_eq!(flac.frame_samples(), 240);
        assert_eq!(opus.microphone_stream().frame_samples(), 480);
    }

    #[test]
    fn microphone_parses_from_toml() {
        let parsed: AudioConfig = toml::from_str(
            "[microphone]\nenabled = true\nbitrate_kbps = 24\nframe_duration = \"20ms\"\njitter_buffer_ms = 40",
        )
        .unwrap();

        assert!(parsed.microphone.enabled);
        assert_eq!(parsed.microphone.bitrate_kbps, 24);
        assert_eq!(parsed.microphone.frame_duration, OpusFrameDuration::Ms20);
        assert_eq!(parsed.microphone.jitter_buffer_ms, 40);
    }

    #[test]
    fn invalid_microphone_settings_are_rejected() {
        let with = |microphone: MicrophoneConfig| AudioConfig {
            microphone,
            ..AudioConfig::default()
        };

        assert!(matches!(
            with(MicrophoneConfig {
                bitrate_kbps: 0,
                ..MicrophoneConfig::default()
            })
            .validate(),
            Err(ConfigError::ZeroMicrophoneBitrate)
        ));
        assert!(matches!(
            with(MicrophoneConfig {
                jitter_buffer_ms: 501,
                ..MicrophoneConfig::default()
            })
            .validate(),
            Err(ConfigError::MicrophoneJitterBufferOutOfRange(501))
        ));
        assert!(matches!(
            with(MicrophoneConfig {
                fec_overhead_percent: 101,
                ..MicrophoneConfig::default()
            })
            .validate(),
            Err(ConfigError::MicrophoneFecOverheadOutOfRange(101))
        ));
    }
}
