use rusplay_core::{ChromaSubsampling, Resolution, ScaleFilter};
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
    #[error("hdr reference white {0} nits is outside 1..=10000")]
    ReferenceWhiteOutOfRange(u32),
    #[error("hdr peak luminance {0} nits is outside 1..=10000")]
    PeakLuminanceOutOfRange(u32),
    #[error("row alignment {0} is not a power of two of at least 4 bytes")]
    RowAlignmentInvalid(u32),
    #[error("a frame of {bytes} bytes exceeds the addressable 16 GiB")]
    FrameTooLarge { bytes: u64 },
    #[error("reference white {0} cd/m² is outside 0..=10000")]
    ReferenceWhiteNitsOutOfRange(f64),
    #[error("peak luminance {0} cd/m² is outside 0..=10000")]
    PeakNitsOutOfRange(f64),
    #[error("black level {black_nits} cd/m² is not between 0 and the peak {peak_nits} cd/m²")]
    BlackNitsOutOfRange { black_nits: f64, peak_nits: f64 },
    #[error("gamut mapping knee {0} is outside 0.0..1.0")]
    GamutKneeOutOfRange(f64),
    #[error("a colour conversion matrix is singular")]
    SingularColorMatrix,
    #[error("the yuv frame was created for a different layout than the converter's")]
    FrameLayoutMismatch,
}
