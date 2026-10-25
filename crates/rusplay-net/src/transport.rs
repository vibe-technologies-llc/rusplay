use std::{
    io,
    net::{SocketAddr, UdpSocket},
};

use rusplay_config::NetworkConfig;
use rustix::net::sockopt;

use crate::NetError;

const IPV4_UDP_OVERHEAD: usize = 28;
const IPV6_UDP_OVERHEAD: usize = 48;

pub fn max_datagram_len(mtu: u16, peer: &SocketAddr) -> usize {
    let overhead = if peer.is_ipv6() {
        IPV6_UDP_OVERHEAD
    } else {
        IPV4_UDP_OVERHEAD
    };

    usize::from(mtu).saturating_sub(overhead)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportOptions {
    pub mtu: u16,
    pub socket_buffer_bytes: u32,
}

impl Default for TransportOptions {
    fn default() -> Self {
        Self::from(&NetworkConfig::default())
    }
}

impl From<&NetworkConfig> for TransportOptions {
    fn from(config: &NetworkConfig) -> Self {
        Self {
            mtu: config.mtu,
            socket_buffer_bytes: config.socket_buffer_bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketBuffers {
    pub send_bytes: usize,
    pub receive_bytes: usize,
}

#[derive(Debug)]
pub struct Transport {
    socket: UdpSocket,
    mtu: u16,
}

impl Transport {
    pub fn bind(addr: SocketAddr, options: TransportOptions) -> Result<Self, NetError> {
        let socket = UdpSocket::bind(addr).map_err(|source| NetError::Bind { addr, source })?;

        if options.socket_buffer_bytes > 0 {
            let requested = options.socket_buffer_bytes as usize;

            sockopt::set_socket_send_buffer_size(&socket, requested).map_err(|errno| {
                NetError::SendBufferSize {
                    requested,
                    source: errno.into(),
                }
            })?;
            sockopt::set_socket_recv_buffer_size(&socket, requested).map_err(|errno| {
                NetError::ReceiveBufferSize {
                    requested,
                    source: errno.into(),
                }
            })?;
        }

        Ok(Self {
            socket,
            mtu: options.mtu,
        })
    }

    pub fn socket_buffers(&self) -> Result<SocketBuffers, NetError> {
        let query = |errno: rustix::io::Errno| NetError::BufferSizeQuery(errno.into());

        Ok(SocketBuffers {
            send_bytes: sockopt::socket_send_buffer_size(&self.socket).map_err(query)?,
            receive_bytes: sockopt::socket_recv_buffer_size(&self.socket).map_err(query)?,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.socket.local_addr().map_err(NetError::LocalAddr)
    }

    pub const fn mtu(&self) -> u16 {
        self.mtu
    }

    pub(crate) fn socket(&self) -> &UdpSocket {
        &self.socket
    }

    pub fn max_datagram_len(&self, peer: &SocketAddr) -> usize {
        max_datagram_len(self.mtu, peer)
    }

    pub fn send_to(&self, datagram: &[u8], peer: SocketAddr) -> Result<(), NetError> {
        if datagram.len() > self.max_datagram_len(&peer) {
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

    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<(), NetError> {
        self.socket
            .set_nonblocking(nonblocking)
            .map_err(NetError::NonBlocking)
    }

    pub fn try_send_to(&self, datagram: &[u8], peer: SocketAddr) -> Result<bool, NetError> {
        match self.send_to(datagram, peer) {
            Err(NetError::Send { source, .. }) if source.kind() == io::ErrorKind::WouldBlock => {
                Ok(false)
            }
            other => other.map(|()| true),
        }
    }

    pub fn try_recv_from(&self, buf: &mut [u8]) -> Result<Option<(usize, SocketAddr)>, NetError> {
        match self.socket.recv_from(buf) {
            Ok(received) => Ok(Some(received)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(NetError::Receive(error)),
        }
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), NetError> {
        self.socket.recv_from(buf).map_err(NetError::Receive)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    const LOOPBACK_V4: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

    #[test]
    fn datagram_budget_leaves_room_for_ip_and_udp_headers() {
        let v6 = SocketAddr::new(std::net::IpAddr::V6(Ipv6Addr::LOCALHOST), 0);

        assert_eq!(max_datagram_len(1500, &LOOPBACK_V4), 1472);
        assert_eq!(max_datagram_len(1500, &v6), 1452);
        assert_eq!(max_datagram_len(10, &v6), 0);
    }

    fn options(mtu: u16) -> TransportOptions {
        TransportOptions {
            mtu,
            ..TransportOptions::default()
        }
    }

    #[test]
    fn configured_buffer_size_reaches_the_socket() {
        let transport = Transport::bind(
            LOOPBACK_V4,
            TransportOptions {
                mtu: 1500,
                socket_buffer_bytes: 65_536,
            },
        )
        .unwrap();

        let buffers = transport.socket_buffers().unwrap();

        assert!(buffers.send_bytes >= 65_536);
        assert!(buffers.receive_bytes >= 65_536);
    }

    #[test]
    fn zero_buffer_size_keeps_the_kernel_default() {
        let default = Transport::bind(LOOPBACK_V4, options(1500)).unwrap();
        let untouched = Transport::bind(
            LOOPBACK_V4,
            TransportOptions {
                mtu: 1500,
                socket_buffer_bytes: 0,
            },
        )
        .unwrap();
        let plain = UdpSocket::bind(LOOPBACK_V4).unwrap();

        assert_eq!(
            untouched.socket_buffers().unwrap().receive_bytes,
            sockopt::socket_recv_buffer_size(&plain).unwrap()
        );
        assert!(default.socket_buffers().unwrap().receive_bytes > 0);
    }

    #[test]
    fn options_follow_the_network_config() {
        let config = NetworkConfig {
            mtu: 1400,
            socket_buffer_bytes: 123_456,
            ..NetworkConfig::default()
        };

        assert_eq!(
            TransportOptions::from(&config),
            TransportOptions {
                mtu: 1400,
                socket_buffer_bytes: 123_456,
            }
        );
    }

    #[test]
    fn oversized_datagram_is_refused_before_sending() {
        let transport = Transport::bind(LOOPBACK_V4, options(576)).unwrap();
        let peer = transport.local_addr().unwrap();

        let result = transport.send_to(&vec![0; 549], peer);

        assert!(matches!(
            result,
            Err(NetError::DatagramTooLarge {
                size: 549,
                mtu: 576
            })
        ));
    }

    #[test]
    fn datagram_at_the_budget_travels_over_loopback() {
        let transport = Transport::bind(LOOPBACK_V4, options(576)).unwrap();
        let peer = transport.local_addr().unwrap();
        let datagram = vec![7; transport.max_datagram_len(&peer)];

        transport.send_to(&datagram, peer).unwrap();

        let mut buf = [0; 2048];
        let (len, from) = transport.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], datagram.as_slice());
        assert_eq!(from, peer);
    }
}
