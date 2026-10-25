mod backend;
mod error;

use rusplay_capture::Frame;
use rusplay_config::VideoConfig;
use rusplay_core::EncodedFrame;

pub use crate::{backend::Backend, error::EncodeError};

pub trait VideoEncoder {
    fn encode(&mut self, frame: &Frame) -> Result<Option<EncodedFrame>, EncodeError>;

    fn request_keyframe(&mut self);

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), EncodeError>;
}

pub fn open(config: &VideoConfig) -> Result<Box<dyn VideoEncoder>, EncodeError> {
    let backend = Backend::from(&config.encoder);

    backend.check_codec(config.codec)?;
    Err(EncodeError::BackendNotCompiled { backend })
}
