use std::{fmt, io, path::PathBuf};

use rusplay_capture::DrmFourcc;
use rusplay_config::RateControl;
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
    #[error("the installed {backend} library does not match the headers rusplay was built against")]
    LibraryIncompatible { backend: Backend },
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
