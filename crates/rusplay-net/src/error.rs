use std::{io, net::SocketAddr};

use rusplay_protocol::ProtocolError;
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
    #[error("datagram of {size} bytes exceeds the {mtu} byte mtu")]
    DatagramTooLarge { size: usize, mtu: u16 },
    #[error("reed-solomon coding failed")]
    FecCoding(#[from] reed_solomon_simd::Error),
    #[error("fec block expects {expected} data shards but {found} were given")]
    FecShardCount { expected: usize, found: usize },
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}
