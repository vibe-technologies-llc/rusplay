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
    #[error("unknown reconnect message tag {0}")]
    UnknownReconnectTag(u8),
    #[error("unknown reconnect rejection reason {0}")]
    UnknownReconnectRejection(u8),
    #[error("unknown stream kind {0}")]
    UnknownStreamKind(u8),
    #[error("cursor flags {0:#04x} carry unknown bits")]
    UnknownCursorFlags(u8),
    #[error("cursor image holds {found} pixel bytes, its size needs {expected}")]
    CursorImageSizeMismatch { expected: usize, found: usize },
    #[error("input record of {len} bytes exceeds the limit of {max}")]
    InputRecordTooLong { len: usize, max: usize },
    #[error("input section of {len} bytes exceeds the 65535 byte limit")]
    InputSectionTooLong { len: usize },
    #[error("unknown codec tag {0}")]
    UnknownCodec(u8),
    #[error("unknown chroma subsampling tag {0}")]
    UnknownChroma(u8),
    #[error("unknown bit depth {0}")]
    UnknownBitDepth(u8),
    #[error("unknown colour primaries code {0}")]
    UnknownColorPrimaries(u8),
    #[error("chromaticity ({x}, {y}) in 0.00002 steps is out of range")]
    ChromaticityOutOfRange { x: u16, y: u16 },
    #[error("the custom primaries do not span a gamut containing their white point")]
    DegenerateGamut,
    #[error("unknown transfer characteristics code {0}")]
    UnknownTransferFunction(u8),
    #[error("unknown matrix coefficients code {0}")]
    UnknownMatrixCoefficients(u8),
    #[error("unknown colour range tag {0}")]
    UnknownColorRange(u8),
    #[error("hdr flags {0:#04x} carry unknown bits")]
    UnknownHdrFlags(u8),
    #[error("luminance of {0} units of 0.0001 cd/m² exceeds 10000 cd/m²")]
    LuminanceOutOfRange(u32),
}
