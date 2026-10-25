use rusplay_capture::DrmFourcc;
use rusplay_core::{BitDepth, ChromaSubsampling, Codec, Resolution};
use thiserror::Error;

use crate::{Backend, input::fourcc_text};

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("the {backend} encoder backend is not compiled into this build")]
    BackendNotCompiled { backend: Backend },
    #[error("the {backend} encoder cannot produce {codec}")]
    CodecUnsupported { backend: Backend, codec: Codec },
    #[error("the {backend} encoder cannot produce {chroma} chroma")]
    ChromaUnsupported {
        backend: Backend,
        chroma: ChromaSubsampling,
    },
    #[error("the {backend} encoder cannot produce {}-bit video", .depth.bits())]
    BitDepthUnsupported { backend: Backend, depth: BitDepth },
    #[error("bitrate of {kbps} kbps is rejected by the {backend} encoder")]
    BitrateRejected { backend: Backend, kbps: u32 },
    #[error("the {backend} encoder does not know the parameter {key}")]
    UnknownParameter { backend: Backend, key: String },
    #[error("the {backend} encoder rejects the value {value:?} for parameter {key}")]
    InvalidParameterValue {
        backend: Backend,
        key: String,
        value: String,
    },
    #[error("the {backend} encoder rejects the tune {tune:?}")]
    TuneRejected { backend: Backend, tune: String },
    #[error("the {backend} encoder could not open at {resolution}")]
    OpenFailed {
        backend: Backend,
        resolution: Resolution,
    },
    #[error("the {backend} encoder failed with code {code}")]
    EncodeFailed { backend: Backend, code: i32 },
    #[error("the {backend} encoder takes CPU frames, not DMA-BUFs")]
    CpuFrameRequired { backend: Backend },
    #[error(
        "the {backend} encoder expects {} frames, got {}",
        fourcc_text(*.expected),
        fourcc_text(*.actual)
    )]
    FrameFormatMismatch {
        backend: Backend,
        expected: DrmFourcc,
        actual: DrmFourcc,
    },
    #[error("the {backend} encoder is open at {expected}, got a {actual} frame")]
    FrameResolutionMismatch {
        backend: Backend,
        expected: Resolution,
        actual: Resolution,
    },
    #[error("frame stride of {actual} bytes is below the {minimum} bytes a row needs")]
    FrameStrideTooSmall { minimum: usize, actual: usize },
    #[error("frame buffer holds {actual} bytes but its planes need {needed}")]
    FrameBufferTooShort { needed: usize, actual: usize },
}
