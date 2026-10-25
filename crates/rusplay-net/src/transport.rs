use std::net::{SocketAddr, UdpSocket};

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

    #[test]
    fn oversized_datagram_is_refused_before_sending() {
        let transport = Transport::bind(LOOPBACK_V4, 576).unwrap();
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
        let transport = Transport::bind(LOOPBACK_V4, 576).unwrap();
        let peer = transport.local_addr().unwrap();
        let datagram = vec![7; transport.max_datagram_len(&peer)];

        transport.send_to(&datagram, peer).unwrap();

        let mut buf = [0; 2048];
        let (len, from) = transport.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], datagram.as_slice());
        assert_eq!(from, peer);
    }
}
