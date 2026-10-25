use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioCodec {
    #[default]
    Opus,
    Flac,
}

impl fmt::Display for AudioCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Opus => "opus",
            Self::Flac => "flac",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum SampleRate {
    #[default]
    #[serde(rename = "48000")]
    Hz48000,
    #[serde(rename = "96000")]
    Hz96000,
}

impl SampleRate {
    pub const fn hz(self) -> u32 {
        match self {
            Self::Hz48000 => 48_000,
            Self::Hz96000 => 96_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelLayout {
    Mono,
    #[default]
    Stereo,
    #[serde(rename = "5.1")]
    Surround51,
    #[serde(rename = "7.1")]
    Surround71,
}

impl ChannelLayout {
    pub const fn channel_count(self) -> u8 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Surround51 => 6,
            Self::Surround71 => 8,
        }
    }
}
