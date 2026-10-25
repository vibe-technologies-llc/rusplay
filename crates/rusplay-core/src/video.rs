use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub const fn pixel_count(self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl FrameRate {
    pub const fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    pub const fn integer(fps: u32) -> Self {
        Self::new(fps, 1)
    }

    pub const fn is_valid(self) -> bool {
        self.numerator != 0 && self.denominator != 0
    }

    pub fn frame_interval(self) -> Option<Duration> {
        self.is_valid()
            .then(|| Duration::from_secs(u64::from(self.denominator)) / self.numerator)
    }
}

impl Default for FrameRate {
    fn default() -> Self {
        Self::integer(60)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Codec {
    H264,
    #[default]
    Hevc,
    Av1,
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChromaSubsampling {
    #[serde(rename = "4:2:0")]
    Yuv420,
    #[default]
    #[serde(rename = "4:2:2")]
    Yuv422,
    #[serde(rename = "4:4:4")]
    Yuv444,
}

impl fmt::Display for ChromaSubsampling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Yuv420 => "4:2:0",
            Self::Yuv422 => "4:2:2",
            Self::Yuv444 => "4:4:4",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BitDepth {
    #[default]
    Eight,
    Ten,
    Twelve,
}

impl BitDepth {
    pub const fn bits(self) -> u8 {
        match self {
            Self::Eight => 8,
            Self::Ten => 10,
            Self::Twelve => 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PixelFormat {
    pub chroma: ChromaSubsampling,
    pub depth: BitDepth,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn frame_interval_of_sixty_fps() {
        let rate = FrameRate::integer(60);

        assert_eq!(
            rate.frame_interval(),
            Some(Duration::from_nanos(16_666_666))
        );
    }

    #[test]
    fn zero_frame_rate_has_no_interval() {
        let rate = FrameRate::new(0, 1);

        assert!(!rate.is_valid());
        assert_eq!(rate.frame_interval(), None);
    }
}
