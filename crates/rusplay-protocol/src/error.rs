use thiserror::Error;

use crate::ProtocolVersion;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProtocolError {
    #[error("buffer holds {available} bytes but {needed} are required")]
    BufferTooShort { needed: usize, available: usize },
    #[error("peer speaks protocol {found:?}, expected {expected:?}")]
    VersionMismatch {
        expected: ProtocolVersion,
        found: ProtocolVersion,
    },
    #[error("fragment index {index} is outside the fragment count {count}")]
    FragmentOutOfRange { index: u16, count: u16 },
    #[error(
        "fec geometry of {data_shards} data and {parity_shards} parity shards is invalid for parity index {parity_index}"
    )]
    InvalidFecGeometry {
        data_shards: u8,
        parity_shards: u8,
        parity_index: u8,
    },
    #[error("unknown stream kind {0}")]
    UnknownStreamKind(u8),
}
