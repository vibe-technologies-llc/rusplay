use std::{fmt, io};

use rusplay_config::RateControl;
use rusplay_core::{BitDepth, ChromaSubsampling, Codec, ColorPrimaries, DrmFourcc, Resolution};
#[cfg(feature = "nvenc")]
use rusplay_cuda::CudaError;
#[cfg(feature = "vaapi")]
use rusplay_va::VaError;
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
    #[error("the {backend} encoder cannot signal {primaries} colour primaries")]
    ColorPrimariesUnsignalable {
        backend: Backend,
        primaries: ColorPrimaries,
    },
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
    #[error("the installed {backend} library does not match the headers rusplay was built against")]
    LibraryIncompatible { backend: Backend },
    #[error("the {backend} encoder failed with code {code}")]
    EncodeFailed { backend: Backend, code: i32 },
    #[error("the {backend} encoder takes CPU frames, not GPU memory")]
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
    #[error("frame stride of {stride} bytes is not a multiple of the {alignment} byte sample size")]
    FrameStrideMisaligned { stride: usize, alignment: usize },
    #[error("frame buffer holds {actual} bytes but its planes need {needed}")]
    FrameBufferTooShort { needed: usize, actual: usize },
    #[error("the {backend} encoder does not support {rate_control:?} rate control")]
    RateControlUnsupported {
        backend: Backend,
        rate_control: RateControl,
    },
    #[error("the {backend} encoder cannot encode at {resolution}")]
    ResolutionUnsupported {
        backend: Backend,
        resolution: Resolution,
    },
    #[cfg(feature = "vaapi")]
    #[error(transparent)]
    Va(#[from] VaError),
    #[error("a {} DMA-BUF needs {expected} planes, the frame has {actual}", fourcc_text(*.fourcc))]
    DmaBufPlaneCount {
        fourcc: DrmFourcc,
        expected: usize,
        actual: usize,
    },
    #[error("could not inspect the frame's DMA-BUF")]
    DmaBufInspect(#[source] io::Error),
    #[error("the encoded frame did not fit the {capacity} byte coded buffer")]
    CodedBufferOverflow { capacity: usize },
    #[cfg(feature = "nvenc")]
    #[error(transparent)]
    Cuda(#[from] CudaError),
    #[error("the {backend} driver supports API {supported}, rusplay needs {required}")]
    DriverTooOld {
        backend: Backend,
        required: ApiVersion,
        supported: ApiVersion,
    },
    #[error("the {backend} driver left {function} out of its function table")]
    DriverFunctionMissing {
        backend: Backend,
        function: &'static str,
    },
    #[error("{call} failed with {status}")]
    Nvenc {
        call: NvencCall,
        status: NvencStatus,
    },
    #[error("the {backend} encoder takes only linear DMA-BUFs, got modifier {modifier:#x}")]
    DmaBufModifierUnsupported { backend: Backend, modifier: u64 },
    #[error(
        "plane {plane} of the DMA-BUF has stride {actual}, the {backend} encoder needs {expected}"
    )]
    DmaBufPlaneStride {
        backend: Backend,
        plane: usize,
        expected: u32,
        actual: u32,
    },
    #[error(
        "plane {plane} of the DMA-BUF starts at {actual}, the {backend} encoder needs {expected}"
    )]
    DmaBufPlaneOffset {
        backend: Backend,
        plane: usize,
        expected: u64,
        actual: u32,
    },
    #[error("the DMA-BUF stride {stride} is not a multiple of {alignment}")]
    DmaBufStrideUnaligned { stride: u32, alignment: u32 },
    #[error("the DMA-BUF holds {actual} bytes but its planes need {needed}")]
    DmaBufTooShort { needed: u64, actual: u64 },
    #[error("the {backend} encoder cannot import an opaque Vulkan memory fd")]
    OpaqueFdUnsupported { backend: Backend },
    #[error("the opaque memory holds {actual} bytes but its planes need {needed}")]
    OpaqueFdTooShort { needed: u64, actual: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ApiVersion {
    pub major: u32,
    pub minor: u32,
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvencCall {
    CreateInstance,
    GetMaxSupportedVersion,
    OpenEncodeSessionEx,
    GetEncodeGuids,
    GetInputFormats,
    GetEncodeCaps,
    GetEncodePresetConfigEx,
    InitializeEncoder,
    ReconfigureEncoder,
    CreateInputBuffer,
    LockInputBuffer,
    CreateBitstreamBuffer,
    LockBitstream,
    RegisterResource,
    MapInputResource,
    EncodePicture,
}

impl fmt::Display for NvencCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CreateInstance => "NvEncodeAPICreateInstance",
            Self::GetMaxSupportedVersion => "NvEncodeAPIGetMaxSupportedVersion",
            Self::OpenEncodeSessionEx => "nvEncOpenEncodeSessionEx",
            Self::GetEncodeGuids => "nvEncGetEncodeGUIDs",
            Self::GetInputFormats => "nvEncGetInputFormats",
            Self::GetEncodeCaps => "nvEncGetEncodeCaps",
            Self::GetEncodePresetConfigEx => "nvEncGetEncodePresetConfigEx",
            Self::InitializeEncoder => "nvEncInitializeEncoder",
            Self::ReconfigureEncoder => "nvEncReconfigureEncoder",
            Self::CreateInputBuffer => "nvEncCreateInputBuffer",
            Self::LockInputBuffer => "nvEncLockInputBuffer",
            Self::CreateBitstreamBuffer => "nvEncCreateBitstreamBuffer",
            Self::LockBitstream => "nvEncLockBitstream",
            Self::RegisterResource => "nvEncRegisterResource",
            Self::MapInputResource => "nvEncMapInputResource",
            Self::EncodePicture => "nvEncEncodePicture",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvencStatus(pub i32);

impl NvencStatus {
    const NAMES: [&'static str; 27] = [
        "success",
        "no encode device",
        "unsupported device",
        "invalid encoder device",
        "invalid device",
        "device does not exist",
        "invalid pointer",
        "invalid event",
        "invalid parameter",
        "invalid call",
        "out of memory",
        "encoder not initialized",
        "unsupported parameter",
        "lock busy",
        "not enough buffer",
        "invalid version",
        "map failed",
        "need more input",
        "encoder busy",
        "event not registered",
        "generic error",
        "incompatible client key",
        "unimplemented",
        "resource register failed",
        "resource not registered",
        "resource not mapped",
        "need more output",
    ];
}

impl fmt::Display for NvencStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = usize::try_from(self.0)
            .ok()
            .and_then(|index| Self::NAMES.get(index));
        match name {
            Some(name) => write!(f, "{name} ({})", self.0),
            None => write!(f, "nvenc status {}", self.0),
        }
    }
}
