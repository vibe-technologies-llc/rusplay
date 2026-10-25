mod error;

use std::{fmt, os::fd::OwnedFd};

use rusplay_core::{EncodedFrame, PixelFormat, Resolution, Timestamp};

pub use crate::error::DecodeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Vaapi,
    Nvdec,
    Software,
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

#[derive(Debug)]
pub enum DecodedBuffer {
    DmaBuf { fd: OwnedFd, modifier: u64 },
    Cpu { planes: Vec<Vec<u8>> },
}

#[derive(Debug)]
pub struct DecodedFrame {
    pub resolution: Resolution,
    pub format: PixelFormat,
    pub timestamp: Timestamp,
    pub buffer: DecodedBuffer,
}

pub trait VideoDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<DecodedFrame>, DecodeError>;
}

pub fn open(backend: Backend) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Err(DecodeError::BackendNotCompiled { backend })
}
