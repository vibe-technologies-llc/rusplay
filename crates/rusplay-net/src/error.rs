use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
};

use rusplay_protocol::{PairingRejection, ProtocolError, ReconnectRejection, StreamKind};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetError {
    #[error("failed to bind udp socket on {addr}")]
    Bind { addr: SocketAddr, source: io::Error },
    #[error("failed to send datagram to {peer}")]
    Send { peer: SocketAddr, source: io::Error },
    #[error("failed to query the local socket address")]
    LocalAddr(#[source] io::Error),
    #[error("failed to receive datagram")]
    Receive(#[source] io::Error),
    #[error("failed to set the send buffer to {requested} bytes")]
    SendBufferSize { requested: usize, source: io::Error },
    #[error("failed to set the receive buffer to {requested} bytes")]
    ReceiveBufferSize { requested: usize, source: io::Error },
    #[error("failed to query the socket buffer sizes")]
    BufferSizeQuery(#[source] io::Error),
    #[error("failed to switch the socket blocking mode")]
    NonBlocking(#[source] io::Error),
    #[error("control channel already holds {window} unacknowledged messages")]
    ControlWindowFull { window: u32 },
    #[error("failed to join multicast group {group}")]
    MulticastJoin { group: Ipv4Addr, source: io::Error },
    #[error("failed to set the socket read timeout")]
    ReadTimeout(#[source] io::Error),
    #[error("failed to enable broadcast on the socket")]
    Broadcast(#[source] io::Error),
    #[error("failed to set the multicast ttl")]
    MulticastTtl(#[source] io::Error),
    #[error("sealed datagram of {len} bytes is shorter than the {min} byte minimum")]
    SessionDatagramTooShort { len: usize, min: usize },
    #[error("failed to seal a datagram")]
    SessionSeal(#[source] chacha20poly1305::Error),
    #[error("datagram failed authentication")]
    SessionAuthentication,
    #[error("datagram counter {counter} was already seen or is too old")]
    SessionReplay { counter: u64 },
    #[error("the system random source failed")]
    RandomUnavailable(#[source] io::Error),
    #[error("pin must be {min} to {max} ascii digits, found {len} characters")]
    InvalidPin { len: usize, min: u8, max: u8 },
    #[error("pairing key exchange failed")]
    PairingKeyExchange(#[source] spake2::Error),
    #[error("pairing key derivation failed")]
    PairingKeyDerivation(#[source] hkdf::InvalidLength),
    #[error("peer proved it does not know the pin")]
    PairingAuthentication,
    #[error("peer rejected the pairing: {reason:?}")]
    PairingRejected { reason: PairingRejection },
    #[error("pairing is locked after too many attempts")]
    PairingLocked,
    #[error("pairing did not finish in time")]
    PairingTimeout,
    #[error("reconnect key derivation failed")]
    ReconnectKeyDerivation(#[source] hkdf::InvalidLength),
    #[error("host did not prove it knows the remembered secret")]
    ReconnectAuthentication,
    #[error("peer rejected the reconnect: {reason:?}")]
    ReconnectRejected { reason: ReconnectRejection },
    #[error("reconnect did not finish in time")]
    ReconnectTimeout,
    #[error("peer name must not be empty")]
    PeerNameEmpty,
    #[error("peer name of {len} bytes exceeds the limit of {max}")]
    PeerNameTooLong { len: usize, max: usize },
    #[error("peer name contains a control character")]
    PeerNameControlCharacter,
    #[error("neither XDG_STATE_HOME nor HOME gives an absolute state directory")]
    NoStateDirectory,
    #[error("failed to read the peer store {path}")]
    PeerStoreRead { path: PathBuf, source: io::Error },
    #[error("failed to write the peer store {path}")]
    PeerStoreWrite { path: PathBuf, source: io::Error },
    #[error("peer store {path} is corrupt at line {line}")]
    PeerStoreCorrupt { path: PathBuf, line: usize },
    #[error("datagram of {size} bytes exceeds the {mtu} byte mtu")]
    DatagramTooLarge { size: usize, mtu: u16 },
    #[error("reed-solomon coding failed")]
    FecCoding(#[from] reed_solomon_simd::Error),
    #[error("fec block expects {expected} data shards but {found} were given")]
    FecShardCount { expected: usize, found: usize },
    #[error("stream {0:?} does not carry fragmented frames")]
    UnfragmentableStream(StreamKind),
    #[error("a datagram budget of {max_datagram} bytes leaves no room for payload")]
    DatagramBudgetTooSmall { max_datagram: usize },
    #[error("frames must not be empty")]
    EmptyFrame,
    #[error("frame of {bytes} bytes exceeds the {max_bytes} byte limit")]
    FrameTooLarge { bytes: usize, max_bytes: usize },
    #[error("datagram belongs to stream {found:?}, expected {expected:?}")]
    StreamMismatch {
        expected: StreamKind,
        found: StreamKind,
    },
    #[error("frame announces {count} fragments, above the limit of {max}")]
    FragmentCountTooLarge { count: u32, max: u16 },
    #[error("frame {frame} was announced with {expected} fragments but a packet says {found}")]
    FragmentCountMismatch {
        frame: u32,
        expected: u16,
        found: u16,
    },
    #[error("shard length {0} is not a positive even number")]
    InvalidShardLength(usize),
    #[error("frame uses {expected} byte shards but a packet carries {found}")]
    ShardLengthMismatch { expected: usize, found: usize },
    #[error("frame is {expected} bytes long but a packet says {found}")]
    FrameBytesMismatch { expected: u32, found: u32 },
    #[error("frame announces zero bytes")]
    ZeroFrameBytes,
    #[error("fragment {index} of frame {frame} has an impossible length of {len} bytes")]
    FragmentLengthMismatch { frame: u32, index: u16, len: usize },
    #[error(
        "fec block of {data_shards} shards at fragment {first_fragment} exceeds {fragment_count} fragments"
    )]
    FecBlockOutOfRange {
        first_fragment: u16,
        data_shards: u8,
        fragment_count: u16,
    },
    #[error("fec block at fragment {first_fragment} conflicts with a known block")]
    FecBlockConflict { first_fragment: u16 },
    #[error("frame {frame} assembled to {assembled} bytes but was announced as {expected}")]
    FrameLengthMismatch {
        frame: u32,
        expected: u32,
        assembled: usize,
    },
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}
