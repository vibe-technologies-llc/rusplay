mod error;
mod frame;
#[cfg(feature = "software")]
mod software;

use std::fmt;

use rusplay_core::EncodedFrame;

#[cfg(feature = "software")]
pub use crate::software::Rav1dDecoder;
pub use crate::{
    error::DecodeError,
    frame::{
        CpuFrame, CpuPlane, DecodedBuffer, DecodedFrame, DmaBufFrame, DmaBufImage, DmaBufObject,
        DmaBufPlane, DrmFourcc,
    },
};

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

pub trait VideoDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<DecodedFrame>, DecodeError>;
}

pub fn open(backend: Backend) -> Result<Box<dyn VideoDecoder>, DecodeError> {
    Err(DecodeError::BackendNotCompiled { backend })
}
