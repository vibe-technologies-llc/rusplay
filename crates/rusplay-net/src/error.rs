use std::{io, net::SocketAddr};

use rusplay_protocol::{ProtocolError, StreamKind};
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
