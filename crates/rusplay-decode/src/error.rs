use std::{fmt, io};

use rusplay_core::{ChromaSubsampling, Codec};
#[cfg(feature = "nvdec")]
use rusplay_cuda::{CudaError, CudaStatus};
#[cfg(feature = "vaapi")]
use rusplay_va::VaError;
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
    #[cfg(feature = "vaapi")]
    #[error(transparent)]
    Va(#[from] VaError),
    #[error("the vaapi driver has no decoder for {codec} profile {profile}")]
    ProfileUnsupported { codec: Codec, profile: u8 },
    #[error("malformed {codec} {unit}: {reason}")]
    Malformed {
        codec: Codec,
        unit: BitstreamUnit,
        reason: String,
    },
    #[error(
        "{codec} crops {left} columns from the left and {top} rows from the top, which the decoder cannot carry to the client"
    )]
    CropOriginUnsupported { codec: Codec, left: u64, top: u64 },
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
    #[cfg(feature = "nvdec")]
    #[error(transparent)]
    Cuda(#[from] CudaError),
    #[cfg(feature = "nvdec")]
    #[error("{call} failed with {status}")]
    Cuvid { call: CuvidCall, status: CudaStatus },
    #[error("the {backend} decoder cannot decode {codec} at {width}x{height}")]
    ResolutionUnsupported {
        backend: Backend,
        codec: Codec,
        width: u32,
        height: u32,
    },
    #[error("the nvdec parser delivered a {codec} picture before its sequence")]
    PictureBeforeSequence { codec: Codec },
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

#[cfg(feature = "nvdec")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CuvidCall {
    GetDecoderCaps,
    CreateDecoder,
    DecodePicture,
    MapVideoFrame,
    CreateVideoParser,
    ParseVideoData,
}

#[cfg(feature = "nvdec")]
impl fmt::Display for CuvidCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GetDecoderCaps => "cuvidGetDecoderCaps",
            Self::CreateDecoder => "cuvidCreateDecoder",
            Self::DecodePicture => "cuvidDecodePicture",
            Self::MapVideoFrame => "cuvidMapVideoFrame64",
            Self::CreateVideoParser => "cuvidCreateVideoParser",
            Self::ParseVideoData => "cuvidParseVideoData",
        })
    }
}

#[cfg(test)]
impl DecodeError {
    pub(crate) fn is_device_open(&self) -> bool {
        #[cfg(feature = "vaapi")]
        {
            matches!(self, Self::Va(VaError::DeviceOpen { .. }))
        }
        #[cfg(not(feature = "vaapi"))]
        {
            false
        }
    }
}
