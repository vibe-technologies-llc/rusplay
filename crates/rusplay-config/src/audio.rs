use rusplay_core::{AudioCodec, ChannelLayout, SampleRate};
use serde::{Deserialize, Serialize};

use crate::ConfigError;

const MAX_FLAC_COMPRESSION: u8 = 8;
const MAX_JITTER_BUFFER_MS: u32 = 500;
const FLAC_BLOCK_SIZES: std::ops::RangeInclusive<u16> = 32..=32767;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub codec: AudioCodec,
    pub sample_rate: SampleRate,
    pub channels: ChannelLayout,
    pub jitter_buffer_ms: u32,
    pub opus: OpusConfig,
    pub flac: FlacConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            codec: AudioCodec::default(),
            sample_rate: SampleRate::default(),
            channels: ChannelLayout::default(),
            jitter_buffer_ms: 10,
            opus: OpusConfig::default(),
            flac: FlacConfig::default(),
        }
    }
}

impl AudioConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.opus.bitrate_kbps == 0 {
            return Err(ConfigError::ZeroAudioBitrate);
        }

        if self.flac.compression_level > MAX_FLAC_COMPRESSION {
            return Err(ConfigError::FlacCompressionOutOfRange(
                self.flac.compression_level,
            ));
        }

        if self.jitter_buffer_ms > MAX_JITTER_BUFFER_MS {
            return Err(ConfigError::JitterBufferOutOfRange(self.jitter_buffer_ms));
        }

        if !FLAC_BLOCK_SIZES.contains(&self.flac.block_size) {
            return Err(ConfigError::FlacBlockSizeOutOfRange(self.flac.block_size));
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
}
