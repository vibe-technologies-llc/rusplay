use std::io;

use thiserror::Error;

use crate::DrmFourcc;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("screencast portal request failed")]
    Portal(#[from] ashpd::Error),
    #[error("the portal returned no streams")]
    NoStreams,
    #[error("the pipewire stream ended")]
    StreamEnded,
    #[cfg(feature = "pipewire")]
    #[error("pipewire call failed")]
    PipeWire(#[from] pipewire::Error),
    #[error("the pipewire stream entered the error state: {message}")]
    StreamFailed { message: String },
    #[error("the pipewire thread could not be spawned")]
    ThreadSpawn(#[source] io::Error),
    #[error("the pipewire thread exited before the stream connected")]
    ThreadExited,
    #[error("failed to serialize the spa param {param}")]
    PodSerialize { param: u32 },
    #[error("the negotiated format pod could not be parsed")]
    FormatPodMalformed,
    #[error("the negotiated format lacks spa property {key}")]
    FormatPropertyMissing { key: u32 },
    #[error("spa video format {spa_format} has no drm fourcc mapping")]
    UnsupportedVideoFormat { spa_format: u32 },
    #[error("a frame arrived before the format was negotiated")]
    FormatNotNegotiated,
    #[error("buffer data of type {data_type} is not supported")]
    UnsupportedBufferData { data_type: u32 },
    #[error("buffer chunk at offset {offset} with size {size} exceeds the {mapped} mapped bytes")]
    ChunkOutOfBounds {
        offset: u32,
        size: u32,
        mapped: usize,
    },
    #[error("buffer stride {stride} is invalid for width {width} of format {format:?}")]
    InvalidStride {
        stride: i32,
        width: u32,
        format: DrmFourcc,
    },
}
