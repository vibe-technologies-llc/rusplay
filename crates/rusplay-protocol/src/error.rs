use thiserror::Error;

use crate::{ProtocolVersion, StreamKind};

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
    #[error("unknown control message tag {0}")]
    UnknownControlTag(u8),
    #[error("{extra} unexpected bytes follow the message")]
    TrailingBytes { extra: usize },
    #[error("datagram belongs to stream {found:?}, expected {expected:?}")]
    UnexpectedStream {
        expected: StreamKind,
        found: StreamKind,
    },
    #[error("unknown discovery message tag {0}")]
    UnknownDiscoveryTag(u8),
    #[error("host name of {len} bytes exceeds the limit of {max}")]
    DiscoveryNameTooLong { len: usize, max: usize },
    #[error("host name is not valid utf-8")]
    InvalidDiscoveryName,
    #[error("unknown pairing message tag {0}")]
    UnknownPairingTag(u8),
    #[error("unknown pairing rejection reason {0}")]
    UnknownPairingRejection(u8),
    #[error("unknown stream kind {0}")]
    UnknownStreamKind(u8),
}
