#![recursion_limit = "256"]

#[cfg(test)]
mod bench;
mod bessel;
mod color;
mod convert;
mod dmabuf;
mod error;
mod export;
mod fault;
mod gpu;
#[cfg(test)]
mod hardware;
mod kernel;
mod linearise;
mod polar;
#[cfg(test)]
mod reference;
mod scaler;
mod shaping;
mod sigmoid;
mod table;
mod taps;
#[cfg(test)]
mod testing;
#[cfg(any(test, feature = "udmabuf"))]
pub mod udmabuf;
mod unpack;

pub use crate::{
    color::{
        ColorChain, ColorChainSettings, ColorLut, ColorLutSettings, ColorMapSettings, ColorMapper,
        ColorPlan, ColorVolume, DEFAULT_LUT_SIZE, DynamicPeak, GamutMapping, LUT_SIZES,
        MappingPath, ToneCurve, ToneMapping, ToneStage, WorkingSpace,
    },
    convert::{
        ChromaSiting, ColorConverter, ConvertSettings, DEFAULT_ROW_ALIGNMENT, FrameLayout,
        InputLight, Plane, PlaneContent, PlaneLayout, Quantization, SampleAlignment, YuvFrame,
    },
    dmabuf::{
        DMABUF_FEATURES, DmaBuf, DmaBufFormat, DmaBufImporter, DmaBufPlane, DmaBufTexture,
        GpuFenceWait, GpuFenceWaiter, IMPORT_CACHE_ENTRIES, WriteFence, device_descriptor,
        release_after_submission, request_fence_wait_device, wait_for_writes,
    },
    error::ScaleError,
    export::{ExportedFrame, MemoryHandle, can_export},
    fault::{GpuFault, GpuFaults, check_extent},
    gpu::ScalePipelines,
    kernel::{Geometry, Kernel},
    linearise::{ChannelOrder, LineariseSettings, Lineariser, SourceChannels, TexelPacking},
    scaler::{Reconfigured, ScaleSettings, Scaler},
    sigmoid::Sigmoid,
    table::TapTable,
    taps::Taps,
    unpack::{OutputLight, RgbConvertSettings, RgbConverter},
};
