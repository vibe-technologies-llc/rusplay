use rusplay_core::{ChromaSubsampling, Resolution, ScaleFilter, TransferFunction};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ScaleError {
    #[error("source resolution {0} has no pixels")]
    EmptySource(Resolution),
    #[error("target resolution {0} has no pixels")]
    EmptyTarget(Resolution),
    #[error("antiringing strength {0} is outside 0.0..=1.0")]
    AntiringingOutOfRange(f32),
    #[error("sigmoid center {0} is outside 0.0..=1.0")]
    SigmoidCenterOutOfRange(f32),
    #[error("sigmoid slope {0} is outside 1.0..=20.0")]
    SigmoidSlopeOutOfRange(f32),
    #[error(
        "chroma filter {filter} is polar and cannot downscale {chroma} chroma, which keeps full vertical resolution"
    )]
    PolarChromaFilterUnsupported {
        filter: ScaleFilter,
        chroma: ChromaSubsampling,
    },
    #[error("encoding linear light with the {0:?} transfer function is not supported")]
    TransferUnsupported(TransferFunction),
    #[error("row alignment {0} is not a power of two of at least 4 bytes")]
    RowAlignmentInvalid(u32),
    #[error("a frame of {bytes} bytes exceeds the addressable 16 GiB")]
    FrameTooLarge { bytes: u64 },
    #[error("the yuv frame was created for a different layout than the converter's")]
    FrameLayoutMismatch,
}
