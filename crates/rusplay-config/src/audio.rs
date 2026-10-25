use rusplay_core::{AudioCodec, ChannelLayout, SampleRate};
use serde::{Deserialize, Serialize};

use crate::ConfigError;

const MAX_FLAC_COMPRESSION: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub codec: AudioCodec,
    pub sample_rate: SampleRate,
    pub channels: ChannelLayout,
    pub opus: OpusConfig,
    pub flac: FlacConfig,
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
