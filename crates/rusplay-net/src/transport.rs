use std::net::{SocketAddr, UdpSocket};

use crate::NetError;

#[derive(Debug)]
pub struct Transport {
    socket: UdpSocket,
    mtu: u16,
}

impl Transport {
    pub fn bind(addr: SocketAddr, mtu: u16) -> Result<Self, NetError> {
        let socket = UdpSocket::bind(addr).map_err(|source| NetError::Bind { addr, source })?;

        Ok(Self { socket, mtu })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.socket.local_addr().map_err(NetError::LocalAddr)
    }

    pub fn send_to(&self, datagram: &[u8], peer: SocketAddr) -> Result<(), NetError> {
        if datagram.len() > usize::from(self.mtu) {
            return Err(NetError::DatagramTooLarge {
                size: datagram.len(),
                mtu: self.mtu,
            });
        }

        self.socket
            .send_to(datagram, peer)
            .map(drop)
            .map_err(|source| NetError::Send { peer, source })
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), NetError> {
        self.socket.recv_from(buf).map_err(NetError::Receive)
    }
}
