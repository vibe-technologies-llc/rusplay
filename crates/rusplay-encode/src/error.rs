use rusplay_core::{ChromaSubsampling, Codec};
use thiserror::Error;

use crate::Backend;

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
    #[error("bitrate of {kbps} kbps is rejected by the {backend} encoder")]
    BitrateRejected { backend: Backend, kbps: u32 },
}
