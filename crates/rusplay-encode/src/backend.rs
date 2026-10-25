use std::fmt;

use rusplay_config::VideoEncoderConfig;
use rusplay_core::Codec;

use crate::EncodeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Vaapi,
    Nvenc,
    X264,
    X265,
    SvtAv1,
}

impl Backend {
    pub const fn supports(self, codec: Codec) -> bool {
        match self {
            Self::Vaapi | Self::Nvenc => true,
            Self::X264 => matches!(codec, Codec::H264),
            Self::X265 => matches!(codec, Codec::Hevc),
            Self::SvtAv1 => matches!(codec, Codec::Av1),
        }
    }

    pub fn check_codec(self, codec: Codec) -> Result<(), EncodeError> {
        if self.supports(codec) {
            Ok(())
        } else {
            Err(EncodeError::CodecUnsupported {
                backend: self,
                codec,
            })
        }
    }
}

impl From<&VideoEncoderConfig> for Backend {
    fn from(config: &VideoEncoderConfig) -> Self {
        match config {
            VideoEncoderConfig::Vaapi(_) => Self::Vaapi,
            VideoEncoderConfig::Nvenc(_) => Self::Nvenc,
            VideoEncoderConfig::X264(_) => Self::X264,
            VideoEncoderConfig::X265(_) => Self::X265,
            VideoEncoderConfig::SvtAv1(_) => Self::SvtAv1,
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Vaapi => "vaapi",
            Self::Nvenc => "nvenc",
            Self::X264 => "x264",
            Self::X265 => "x265",
            Self::SvtAv1 => "svt-av1",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_backends_are_single_codec() {
        assert!(Backend::X264.supports(Codec::H264));
        assert!(!Backend::X264.supports(Codec::Hevc));
        assert!(Backend::X265.supports(Codec::Hevc));
        assert!(!Backend::SvtAv1.supports(Codec::H264));
    }
}
