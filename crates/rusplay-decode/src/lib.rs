#[cfg(any(feature = "vaapi", feature = "nvdec"))]
mod annexb;
#[cfg(any(feature = "software", feature = "vaapi", feature = "nvdec"))]
mod av1;
mod capabilities;
mod error;
#[cfg(feature = "ffmpeg")]
mod ffmpeg;
#[cfg(all(test, any(feature = "software", feature = "vaapi", feature = "nvdec")))]
#[path = "../tests/common/mod.rs"]
mod fixtures;
mod frame;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;
#[cfg(any(feature = "vaapi", feature = "nvdec"))]
mod hevc;
#[cfg(any(
    feature = "software",
    feature = "vaapi",
    feature = "nvdec",
    feature = "ffmpeg"
))]
mod metadata;
#[cfg(feature = "nvdec")]
mod nvdec;
#[cfg(any(feature = "vaapi", feature = "nvdec"))]
mod obu;
mod resolve;
#[cfg(any(feature = "vaapi", feature = "nvdec"))]
mod signal;
#[cfg(feature = "software")]
mod software;
#[cfg(feature = "vaapi")]
mod vaapi;

use std::fmt;

use rusplay_config::{
    NvdecDecoderConfig, SoftwareDecoderConfig, VaapiDecoderConfig, VideoDecoderConfig,
};
use rusplay_core::{Codec, EncodedFrame};

#[cfg(feature = "nvdec")]
pub use crate::error::CuvidCall;
#[cfg(feature = "ffmpeg")]
pub use crate::error::FfmpegCall;
#[cfg(feature = "ffmpeg")]
pub use crate::ffmpeg::FfmpegDecoder;
#[cfg(feature = "nvdec")]
pub use crate::nvdec::NvdecDecoder;
#[cfg(feature = "software")]
pub use crate::software::Rav1dDecoder;
#[cfg(feature = "vaapi")]
pub use crate::vaapi::VaapiDecoder;
pub use crate::{
    capabilities::{Capabilities, capabilities},
    error::{Av1SyntaxError, BitstreamUnit, DecodeError},
    frame::{
        CpuFrame, CpuPlane, DecodedBuffer, DecodedFrame, DmaBufFrame, DmaBufImage, DmaBufObject,
        DmaBufPlane, DrmFourcc,
    },
    resolve::resolve,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Vaapi,
    Nvdec,
    Software,
}

impl Backend {
    pub const fn of(config: &VideoDecoderConfig) -> Option<Self> {
        match config {
            VideoDecoderConfig::Auto(_) => None,
            VideoDecoderConfig::Vaapi(_) => Some(Self::Vaapi),
            VideoDecoderConfig::Nvdec(_) => Some(Self::Nvdec),
            VideoDecoderConfig::Software(_) => Some(Self::Software),
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Vaapi => "vaapi",
            Self::Nvdec => "nvdec",
            Self::Software => "software",
        })
    }
}

pub trait VideoDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<DecodedFrame>, DecodeError>;

    fn flush(&mut self) -> Result<Vec<DecodedFrame>, DecodeError> {
        Ok(Vec::new())
    }
}

pub fn open(
    codec: Codec,
    config: &VideoDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    match config {
        VideoDecoderConfig::Auto(_) => match resolve(config, None) {
            VideoDecoderConfig::Auto(_) => Err(DecodeError::NoDecoderAvailable),
            resolved => open(codec, &resolved),
        },
        VideoDecoderConfig::Vaapi(vaapi) => open_vaapi(codec, vaapi),
        VideoDecoderConfig::Nvdec(nvdec) => open_nvdec(codec, nvdec),
        VideoDecoderConfig::Software(software) => open_software(codec, software),
    }
}

#[cfg(feature = "vaapi")]
fn open_vaapi(
    codec: Codec,
    config: &VaapiDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Ok(Box::new(VaapiDecoder::new(codec, config)?))
}

#[cfg(not(feature = "vaapi"))]
fn open_vaapi(
    _codec: Codec,
    _config: &VaapiDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Err(DecodeError::BackendNotCompiled {
        backend: Backend::Vaapi,
    })
}

#[cfg(feature = "nvdec")]
fn open_nvdec(
    codec: Codec,
    config: &NvdecDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Ok(Box::new(NvdecDecoder::new(codec, config)?))
}

#[cfg(not(feature = "nvdec"))]
fn open_nvdec(
    _codec: Codec,
    _config: &NvdecDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Err(DecodeError::BackendNotCompiled {
        backend: Backend::Nvdec,
    })
}

#[cfg(any(feature = "software", feature = "ffmpeg"))]
fn open_software(
    codec: Codec,
    config: &SoftwareDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    match codec {
        Codec::Av1 => open_av1(config),
        Codec::H264 | Codec::Hevc => open_h26x(codec, config),
    }
}

#[cfg(not(any(feature = "software", feature = "ffmpeg")))]
fn open_software(
    _codec: Codec,
    _config: &SoftwareDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Err(DecodeError::BackendNotCompiled {
        backend: Backend::Software,
    })
}

#[cfg(all(any(feature = "software", feature = "ffmpeg"), feature = "software"))]
fn open_av1(config: &SoftwareDecoderConfig) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Ok(Box::new(Rav1dDecoder::new(config)?))
}

#[cfg(all(
    any(feature = "software", feature = "ffmpeg"),
    not(feature = "software")
))]
fn open_av1(_config: &SoftwareDecoderConfig) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Err(DecodeError::CodecUnsupported {
        backend: Backend::Software,
        codec: Codec::Av1,
    })
}

#[cfg(all(any(feature = "software", feature = "ffmpeg"), feature = "ffmpeg"))]
fn open_h26x(
    codec: Codec,
    config: &SoftwareDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Ok(Box::new(FfmpegDecoder::new(codec, config)?))
}

#[cfg(all(any(feature = "software", feature = "ffmpeg"), not(feature = "ffmpeg")))]
fn open_h26x(
    codec: Codec,
    _config: &SoftwareDecoderConfig,
) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Err(DecodeError::CodecUnsupported {
        backend: Backend::Software,
        codec,
    })
}

#[cfg(all(test, feature = "vaapi"))]
mod hardware;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvdec_open_matches_its_feature() {
        let config = VideoDecoderConfig::Nvdec(NvdecDecoderConfig::default());

        let result = open(Codec::Hevc, &config);

        if cfg!(feature = "nvdec") {
            assert!(!matches!(
                result,
                Err(DecodeError::BackendNotCompiled { .. })
            ));
        } else {
            assert!(matches!(
                result,
                Err(DecodeError::BackendNotCompiled {
                    backend: Backend::Nvdec
                })
            ));
        }
    }

    #[test]
    fn software_backend_matches_its_feature() {
        let config = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());

        let av1 = open(Codec::Av1, &config);
        let h264 = open(Codec::H264, &config);

        if cfg!(feature = "software") {
            assert!(av1.is_ok());
        } else if cfg!(feature = "ffmpeg") {
            assert!(matches!(
                av1,
                Err(DecodeError::CodecUnsupported {
                    backend: Backend::Software,
                    codec: Codec::Av1,
                })
            ));
        } else {
            assert!(matches!(
                av1,
                Err(DecodeError::BackendNotCompiled {
                    backend: Backend::Software
                })
            ));
        }

        #[cfg(feature = "ffmpeg")]
        assert!(
            h264.is_ok()
                || matches!(
                    h264,
                    Err(DecodeError::FfmpegDecoderMissing { codec: Codec::H264 })
                )
        );
        #[cfg(all(not(feature = "ffmpeg"), feature = "software"))]
        assert!(matches!(
            h264,
            Err(DecodeError::CodecUnsupported {
                backend: Backend::Software,
                codec: Codec::H264,
            })
        ));
        #[cfg(not(any(feature = "ffmpeg", feature = "software")))]
        assert!(matches!(
            h264,
            Err(DecodeError::BackendNotCompiled {
                backend: Backend::Software
            })
        ));
    }

    #[test]
    fn vaapi_open_matches_its_feature() {
        let config = VideoDecoderConfig::Vaapi(VaapiDecoderConfig {
            device: "/dev/dri/rusplay-missing-node".into(),
            ..VaapiDecoderConfig::default()
        });

        let result = open(Codec::Av1, &config);

        if cfg!(feature = "vaapi") {
            assert!(matches!(&result, Err(error) if error.is_device_open()));
        } else {
            assert!(matches!(
                result,
                Err(DecodeError::BackendNotCompiled {
                    backend: Backend::Vaapi
                })
            ));
        }
    }

    #[test]
    fn backend_follows_the_config() {
        assert_eq!(Backend::of(&VideoDecoderConfig::default()), None);
        assert_eq!(
            Backend::of(&VideoDecoderConfig::Software(
                rusplay_config::SoftwareDecoderConfig::default()
            )),
            Some(Backend::Software)
        );
    }
}
