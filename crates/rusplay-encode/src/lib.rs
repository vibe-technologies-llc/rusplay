mod backend;
mod error;
pub mod input;
#[cfg(any(feature = "x264", feature = "x265", feature = "svt-av1"))]
mod software;
#[cfg(feature = "x264")]
mod x264;

use rusplay_capture::Frame;
use rusplay_config::VideoConfig;
use rusplay_core::EncodedFrame;

pub use crate::{backend::Backend, error::EncodeError};

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
        _ => Err(EncodeError::BackendNotCompiled { backend }),
    }
}
