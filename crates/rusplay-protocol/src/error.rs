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
    #[error("unknown stream kind {0}")]
    UnknownStreamKind(u8),
}
