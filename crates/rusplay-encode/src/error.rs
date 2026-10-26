use std::{fmt, io, path::PathBuf};

use rusplay_capture::DrmFourcc;
use rusplay_config::RateControl;
use rusplay_core::{BitDepth, ChromaSubsampling, Codec, ColorPrimaries, Resolution};
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
    #[error("failed to open the render node {path}")]
    DeviceOpen { path: PathBuf, source: io::Error },
    #[error("libva has no display for the render node {path}")]
    DisplayUnavailable { path: PathBuf },
    #[error("{call} failed with {status}")]
    Va { call: VaCall, status: VaStatus },
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
    #[error("could not load {library}")]
    LibraryNotFound {
        library: &'static str,
        #[source]
        source: libloading::Error,
    },
    #[cfg(feature = "nvenc")]
    #[error("{library} has no symbol {symbol}")]
    SymbolMissing {
        library: &'static str,
        symbol: &'static str,
        #[source]
        source: libloading::Error,
    },
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
    #[error("gpu index {index} is out of range, {count} gpus are present")]
    GpuIndexOutOfRange { index: u32, count: u32 },
    #[error("{call} failed with {status}")]
    Cuda { call: CudaCall, status: CudaStatus },
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
pub enum CudaCall {
    Init,
    DeviceGetCount,
    DeviceGet,
    CtxCreate,
    CtxPushCurrent,
    CtxPopCurrent,
    ImportExternalMemory,
    ExternalMemoryGetMappedBuffer,
}

impl fmt::Display for CudaCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Init => "cuInit",
            Self::DeviceGetCount => "cuDeviceGetCount",
            Self::DeviceGet => "cuDeviceGet",
            Self::CtxCreate => "cuCtxCreate",
            Self::CtxPushCurrent => "cuCtxPushCurrent",
            Self::CtxPopCurrent => "cuCtxPopCurrent",
            Self::ImportExternalMemory => "cuImportExternalMemory",
            Self::ExternalMemoryGetMappedBuffer => "cuExternalMemoryGetMappedBuffer",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CudaStatus(pub i32);

impl CudaStatus {
    const NAMES: [(i32, &'static str); 17] = [
        (1, "invalid value"),
        (2, "out of memory"),
        (3, "not initialized"),
        (4, "deinitialized"),
        (34, "stub library"),
        (35, "insufficient driver"),
        (46, "devices unavailable"),
        (100, "no device"),
        (101, "invalid device"),
        (201, "invalid context"),
        (205, "map failed"),
        (216, "context already in use"),
        (304, "operating system call failed"),
        (400, "invalid handle"),
        (801, "not supported"),
        (802, "system not ready"),
        (999, "unknown error"),
    ];
}

impl fmt::Display for CudaStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match Self::NAMES.iter().find(|(code, _)| *code == self.0) {
            Some((_, name)) => write!(f, "{name} ({})", self.0),
            None => write!(f, "cuda error {}", self.0),
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaCall {
    Initialize,
    QueryConfigProfiles,
    QueryConfigEntrypoints,
    GetConfigAttributes,
    CreateConfig,
    QuerySurfaceAttributes,
    CreateContext,
    CreateSurfaces,
    ExportSurfaceHandle,
    CreateBuffer,
    MapBuffer,
    UnmapBuffer,
    BeginPicture,
    RenderPicture,
    EndPicture,
    SyncSurface,
    CreateImage,
    PutImage,
    GetImage,
}

impl fmt::Display for VaCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Initialize => "vaInitialize",
            Self::QueryConfigProfiles => "vaQueryConfigProfiles",
            Self::QueryConfigEntrypoints => "vaQueryConfigEntrypoints",
            Self::GetConfigAttributes => "vaGetConfigAttributes",
            Self::CreateConfig => "vaCreateConfig",
            Self::QuerySurfaceAttributes => "vaQuerySurfaceAttributes",
            Self::CreateContext => "vaCreateContext",
            Self::CreateSurfaces => "vaCreateSurfaces",
            Self::ExportSurfaceHandle => "vaExportSurfaceHandle",
            Self::CreateBuffer => "vaCreateBuffer",
            Self::MapBuffer => "vaMapBuffer",
            Self::UnmapBuffer => "vaUnmapBuffer",
            Self::BeginPicture => "vaBeginPicture",
            Self::RenderPicture => "vaRenderPicture",
            Self::EndPicture => "vaEndPicture",
            Self::SyncSurface => "vaSyncSurface",
            Self::CreateImage => "vaCreateImage",
            Self::PutImage => "vaPutImage",
            Self::GetImage => "vaGetImage",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaStatus(pub i32);

impl VaStatus {
    const NAMES: [(i32, &'static str); 32] = [
        (0x01, "operation failed"),
        (0x02, "allocation failed"),
        (0x03, "invalid display"),
        (0x04, "invalid config"),
        (0x05, "invalid context"),
        (0x06, "invalid surface"),
        (0x07, "invalid buffer"),
        (0x08, "invalid image"),
        (0x09, "invalid subpicture"),
        (0x0a, "attribute not supported"),
        (0x0b, "maximum number exceeded"),
        (0x0c, "unsupported profile"),
        (0x0d, "unsupported entrypoint"),
        (0x0e, "unsupported rt format"),
        (0x0f, "unsupported buffer type"),
        (0x10, "surface busy"),
        (0x11, "flag not supported"),
        (0x12, "invalid parameter"),
        (0x13, "resolution not supported"),
        (0x14, "unimplemented"),
        (0x15, "surface in displaying"),
        (0x16, "invalid image format"),
        (0x17, "decoding error"),
        (0x18, "encoding error"),
        (0x19, "invalid value"),
        (0x20, "unsupported filter"),
        (0x21, "invalid filter chain"),
        (0x22, "hardware busy"),
        (0x24, "unsupported memory type"),
        (0x25, "not enough buffer"),
        (0x26, "timed out"),
        (-1, "unknown error"),
    ];
}

impl fmt::Display for VaStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match Self::NAMES.iter().find(|(code, _)| *code == self.0) {
            Some((_, name)) => write!(f, "{name} ({:#x})", self.0),
            None => write!(f, "status {:#x}", self.0),
        }
    }
}
