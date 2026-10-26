use ash::vk;
use rusplay_core::{PixelFormat, Resolution, ScaleFilter, TransferFunction};
use rustix::io::Errno;
use thiserror::Error;

use crate::{PlaneLayout, TexelPacking, dmabuf::fourcc_name};

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
    #[error("chroma filter {0} is polar, chroma upsampling needs a separable filter")]
    UpsampleChromaFilterNotSeparable(ScaleFilter),
    #[error("decoding the {0:?} transfer function to linear light is not supported")]
    LinearOutputTransferUnsupported(TransferFunction),
    #[error("a {format:?} texture cannot be read as {packing:?} texels")]
    LineariseSourceFormat {
        format: wgpu::TextureFormat,
        packing: TexelPacking,
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
    #[error("peak detection percentile {0} is outside 0..=100")]
    PeakPercentileOutOfRange(f64),
    #[error("peak detection must smooth over at least one frame")]
    ZeroSmoothingFrames,
    #[error("scene cut threshold {0} dB is not a positive number")]
    SceneThresholdOutOfRange(f64),
    #[error("colour lut size {0} is outside the supported range or the device's 3d texture limit")]
    LutSizeOutOfRange(u32),
    #[error("dynamic peak detection cannot be baked into a colour lut")]
    LutDynamicPeak,
    #[error("highlight gamut mapping is discontinuous and cannot be baked into a colour lut")]
    LutHighlightMapping,
    #[error("a colour lut lookup needs the unpack to output encoded values")]
    LutInputNotEncoded,
    #[error("the colour lut was baked for {lut:?} input, the frame is {frame:?}")]
    LutTransferMismatch {
        lut: TransferFunction,
        frame: TransferFunction,
    },
    #[error("a colour conversion matrix is singular")]
    SingularColorMatrix,
    #[error("the yuv frame was created for a different layout than the converter's")]
    FrameLayoutMismatch,
    #[error(
        "the device was created without dma-buf import (VK_EXT_external_memory_dma_buf and VK_EXT_image_drm_format_modifier)"
    )]
    DmaBufImportUnavailable,
    #[error("dma-buf import needs the vulkan backend")]
    DmaBufNeedsVulkan,
    #[error("dma-buf format {} cannot be imported", fourcc_name(*.0))]
    DmaBufFormatUnsupported(u32),
    #[error("dma-buf format {} with modifier {modifier:#018x} cannot be imported", fourcc_name(*fourcc))]
    DmaBufModifierUnsupported { fourcc: u32, modifier: u64 },
    #[error(
        "dma-buf format {} with modifier {modifier:#018x} needs {expected} planes, the buffer has {found}",
        fourcc_name(*fourcc)
    )]
    DmaBufPlaneCount {
        fourcc: u32,
        modifier: u64,
        expected: usize,
        found: usize,
    },
    #[error("dma-buf plane {plane} lives in another buffer than plane 0")]
    DmaBufPlanesSplit { plane: usize },
    #[error("dma-buf plane {plane} starts at {offset}, past the buffer's {size} bytes")]
    DmaBufPlaneOutOfBounds {
        plane: usize,
        offset: u64,
        size: u64,
    },
    #[error("a {width}x{height} dma-buf plane exceeds what the device can import")]
    DmaBufExtentUnsupported { width: u32, height: u32 },
    #[error("failed to duplicate a dma-buf file descriptor")]
    DmaBufFdDup(#[source] Errno),
    #[error("failed to stat a dma-buf file descriptor")]
    DmaBufFdStat(#[source] Errno),
    #[error("waiting for a dma-buf's write fences failed")]
    DmaBufFencePoll(#[source] Errno),
    #[error("vkCreateImage failed for a dma-buf import")]
    DmaBufImageCreate(#[source] vk::Result),
    #[error("vkGetMemoryFdPropertiesKHR failed for a dma-buf")]
    DmaBufMemoryProperties(#[source] vk::Result),
    #[error("no memory type can hold both the image and the dma-buf")]
    DmaBufNoMemoryType,
    #[error("vkAllocateMemory failed to import a dma-buf")]
    DmaBufMemoryImport(#[source] vk::Result),
    #[error("vkBindImageMemory failed for an imported dma-buf")]
    DmaBufMemoryBind(#[source] vk::Result),
    #[error(
        "dma-buf format {} does not hold {format:?} frames in a {layout:?} layout",
        fourcc_name(*fourcc)
    )]
    DmaBufFormatMismatch {
        fourcc: u32,
        format: PixelFormat,
        layout: PlaneLayout,
    },
    #[error("a {found} dma-buf is smaller than the {needed} frame")]
    DmaBufTooSmall {
        found: Resolution,
        needed: Resolution,
    },
}
