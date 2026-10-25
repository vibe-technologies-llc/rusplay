use rusplay_core::Codec;
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
}
