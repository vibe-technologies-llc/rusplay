use std::io;

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
}
