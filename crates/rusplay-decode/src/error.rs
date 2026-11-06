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
    #[error("vaapi CPU copy does not support image format {fourcc:#010x}")]
    ReadbackFormat { fourcc: u32 },
    #[error("vaapi CPU copy object {object} size {size} exceeds the import limit")]
    ReadbackObjectSize { object: usize, size: u64 },
    #[error("vaapi CPU copy plane {plane} object index {object} exceeds the import limit")]
    ReadbackObjectIndex { plane: usize, object: usize },
    #[error("vaapi CPU copy plane {plane} offset {offset} exceeds the import limit")]
    ReadbackPlaneOffset { plane: usize, offset: u64 },
    #[error("vaapi CPU copy plane {plane} pitch {pitch} exceeds the import limit")]
    ReadbackPlanePitch { plane: usize, pitch: u64 },
    #[error(
        "vaapi CPU copy plane {plane} row at {offset} with {bytes} bytes exceeds its {size} byte image"
    )]
    ReadbackPlane {
        plane: usize,
        offset: usize,
        bytes: usize,
        size: usize,
    },
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
    #[error(transparent)]
    Av1Syntax(#[from] Av1SyntaxError),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Av1SyntaxError {
    #[error("the av1 {unit} ends before its last syntax element")]
    Truncated { unit: BitstreamUnit },
    #[error("av1 sequence profile {0} is reserved")]
    ReservedProfile(u32),
    #[error("an av1 frame header arrived before any sequence header")]
    FrameBeforeSequenceHeader,
    #[error("the av1 frame header uses reference slot {slot}, which holds no known frame")]
    EmptyReferenceSlot { slot: u8 },
    #[error(
        "av1 short reference signalling names slot {slot}, which is not before the current frame"
    )]
    ShortSignalledReferenceNotPast { slot: u8 },
    #[error("the av1 frame uses superres on a {width} pixel wide frame without downscaling it")]
    SuperresWithoutDownscale { width: u32 },
    #[error("{0} av1 tile columns is more than the 64 the format allows")]
    TileColumnsOutOfRange(u32),
    #[error("{0} av1 tile rows is more than the 64 the format allows")]
    TileRowsOutOfRange(u32),
    #[error("av1 context_update_tile_id {tile} is not one of the frame's {tiles} tiles")]
    ContextUpdateTileOutOfRange { tile: u32, tiles: u32 },
    #[error("an av1 frame obu signals show_existing_frame")]
    ShowExistingFrameInFrameObu,
    #[error("a new av1 frame header arrived before the previous frame's last tile")]
    FrameHeaderDuringFrame,
    #[error("an av1 redundant frame header arrived without the frame it repeats")]
    RedundantFrameHeaderWithoutFrame,
    #[error("an av1 tile group arrived without a frame header")]
    TileGroupWithoutFrameHeader,
    #[error(
        "av1 tile group covers tiles {start} to {end}, but the frame expects tile {expected} next of {tiles}"
    )]
    TileGroupOutOfOrder {
        start: u32,
        end: u32,
        expected: u32,
        tiles: u32,
    },
    #[error("av1 tile {tile} claims {size} bytes, but only {remaining} remain in its tile group")]
    TileSizePastData {
        tile: u32,
        size: u64,
        remaining: u64,
    },
    #[error("av1 tile {tile} has no data")]
    EmptyTile { tile: u32 },
    #[error(
        "cros-codecs misreads the frame headers of av1 sequences that carry decoder model info"
    )]
    DecoderModelUnsupported,
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
