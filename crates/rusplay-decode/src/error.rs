use std::{fmt, io, path::PathBuf};

use rusplay_core::{ChromaSubsampling, Codec};
use thiserror::Error;

use crate::Backend;

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("the {backend} decoder backend is not compiled into this build")]
    BackendNotCompiled { backend: Backend },
    #[error("the {backend} decoder cannot decode {codec}")]
    CodecUnsupported { backend: Backend, codec: Codec },
    #[error("received a frame of {found}, but the decoder was opened for {expected}")]
    CodecMismatch { expected: Codec, found: Codec },
    #[error("the {backend} decoder cannot output {codec} with {chroma} chroma")]
    ChromaUnsupported {
        backend: Backend,
        codec: Codec,
        chroma: ChromaSubsampling,
    },
    #[error("the {backend} decoder cannot output {codec} at {bits} bits per sample")]
    BitDepthUnsupported {
        backend: Backend,
        codec: Codec,
        bits: u8,
    },
    #[error(
        "the {backend} decoder received a monochrome {codec} stream, which has no chroma planes"
    )]
    Monochrome { backend: Backend, codec: Codec },
    #[error("{0} software decoder threads is more than rav1d accepts")]
    DecoderThreadsOutOfRange(u32),
    #[error("rav1d failed to open a decoder")]
    Rav1dOpen(#[source] io::Error),
    #[error("rav1d failed to allocate a {size} byte input buffer")]
    Rav1dDataAlloc { size: usize },
    #[error("rav1d rejected the input data")]
    Rav1dSendData(#[source] io::Error),
    #[error("rav1d failed to output a picture")]
    Rav1dGetPicture(#[source] io::Error),
    #[error("failed to open the render node {path}")]
    DeviceOpen { path: PathBuf, source: io::Error },
    #[error("libva has no display for the render node {path}")]
    DisplayUnavailable { path: PathBuf },
    #[error("{call} failed with {status}")]
    Va { call: VaCall, status: VaStatus },
    #[error("the vaapi driver has no decoder for {codec} profile {profile}")]
    ProfileUnsupported { codec: Codec, profile: u8 },
    #[error("interlaced {codec} is not supported")]
    InterlacedUnsupported { codec: Codec },
    #[error("malformed {codec} {unit}: {reason}")]
    Malformed {
        codec: Codec,
        unit: BitstreamUnit,
        reason: String,
    },
    #[error("{codec} {unit} {id} is referenced before it was received")]
    MissingParameterSet {
        codec: Codec,
        unit: BitstreamUnit,
        id: u8,
    },
    #[error("all {capacity} decode surfaces are in use")]
    SurfacePoolExhausted { capacity: usize },
    #[error("the vaapi driver exported a surface as {layers} layers instead of one")]
    ExportLayers { layers: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitstreamUnit {
    NalUnit,
    VideoParameterSet,
    SequenceParameterSet,
    PictureParameterSet,
    SliceHeader,
    Obu,
    SequenceHeader,
    FrameHeader,
    TileGroup,
}

impl fmt::Display for BitstreamUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NalUnit => "nal unit",
            Self::VideoParameterSet => "video parameter set",
            Self::SequenceParameterSet => "sequence parameter set",
            Self::PictureParameterSet => "picture parameter set",
            Self::SliceHeader => "slice header",
            Self::Obu => "obu",
            Self::SequenceHeader => "sequence header",
            Self::FrameHeader => "frame header",
            Self::TileGroup => "tile group",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaCall {
    Initialize,
    QueryConfigProfiles,
    QueryConfigEntrypoints,
    GetConfigAttributes,
    CreateConfig,
    CreateContext,
    CreateSurfaces,
    ExportSurfaceHandle,
    CreateBuffer,
    BeginPicture,
    RenderPicture,
    EndPicture,
    SyncSurface,
    CreateImage,
    GetImage,
    MapBuffer,
}

impl fmt::Display for VaCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Initialize => "vaInitialize",
            Self::QueryConfigProfiles => "vaQueryConfigProfiles",
            Self::QueryConfigEntrypoints => "vaQueryConfigEntrypoints",
            Self::GetConfigAttributes => "vaGetConfigAttributes",
            Self::CreateConfig => "vaCreateConfig",
            Self::CreateContext => "vaCreateContext",
            Self::CreateSurfaces => "vaCreateSurfaces",
            Self::ExportSurfaceHandle => "vaExportSurfaceHandle",
            Self::CreateBuffer => "vaCreateBuffer",
            Self::BeginPicture => "vaBeginPicture",
            Self::RenderPicture => "vaRenderPicture",
            Self::EndPicture => "vaEndPicture",
            Self::SyncSurface => "vaSyncSurface",
            Self::CreateImage => "vaCreateImage",
            Self::GetImage => "vaGetImage",
            Self::MapBuffer => "vaMapBuffer",
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
