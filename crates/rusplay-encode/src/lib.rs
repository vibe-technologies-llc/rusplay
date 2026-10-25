mod backend;
mod error;
pub mod input;
#[cfg(any(
    feature = "x264",
    feature = "x265",
    feature = "svt-av1",
    feature = "vaapi"
))]
mod rate;
#[cfg(any(feature = "x264", feature = "x265", feature = "svt-av1"))]
mod software;
#[cfg(feature = "svt-av1")]
mod svt_av1;
#[cfg(all(test, any(feature = "x264", feature = "x265", feature = "svt-av1")))]
mod testing;
#[cfg(feature = "vaapi")]
mod vaapi;
#[cfg(feature = "x264")]
mod x264;
#[cfg(feature = "x265")]
mod x265;
#[cfg(any(feature = "x264", feature = "x265"))]
mod x26x;

use rusplay_capture::Frame;
use rusplay_config::VideoConfig;
use rusplay_core::EncodedFrame;

pub use crate::{
    backend::Backend,
    error::{EncodeError, VaCall, VaStatus},
};

pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &Frame) -> Result<Option<EncodedFrame>, EncodeError>;

    fn request_keyframe(&mut self);

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), EncodeError>;
}

pub fn open(config: &VideoConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    let backend = Backend::from(&config.encoder);

    backend.check_codec(config.codec)?;

    match &config.encoder {
        #[cfg(feature = "x264")]
        rusplay_config::VideoEncoderConfig::X264(x264) => {
            Ok(Box::new(x264::X264Encoder::new(config, x264)?))
        }
        #[cfg(feature = "x265")]
        rusplay_config::VideoEncoderConfig::X265(x265) => {
            Ok(Box::new(x265::X265Encoder::new(config, x265)?))
        }
        #[cfg(feature = "svt-av1")]
        rusplay_config::VideoEncoderConfig::SvtAv1(svt) => {
            Ok(Box::new(svt_av1::SvtAv1Encoder::new(config, svt)?))
        }
        #[cfg(feature = "vaapi")]
        rusplay_config::VideoEncoderConfig::Vaapi(vaapi) => {
            Ok(Box::new(vaapi::VaapiEncoder::new(config, vaapi)?))
        }
        _ => Err(EncodeError::BackendNotCompiled { backend }),
    }
}
