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
    #[error("cursor flags {0:#04x} carry unknown bits")]
    UnknownCursorFlags(u8),
    #[error("cursor image holds {found} pixel bytes, its size needs {expected}")]
    CursorImageSizeMismatch { expected: usize, found: usize },
    #[error("{0} bytes follow the end of the message")]
    TrailingBytes(usize),
}
