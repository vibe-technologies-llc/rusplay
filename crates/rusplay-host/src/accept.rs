use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use rusplay_config::NetworkConfig;
use rusplay_net::{
    NetError, PairingResponder, PeerRecord, PeerStore, Pin, ReconnectResponder, Transport,
};
use rusplay_protocol::{ClientName, PacketHeader, PeerId, StreamKind};

use crate::HostError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replay {
    pub request: Vec<u8>,
    pub reply: Vec<u8>,
}

impl Replay {
    pub(crate) fn answer(&self, datagram: &[u8]) -> Option<&[u8]> {
        (datagram == self.request.as_slice()).then_some(self.reply.as_slice())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Paired,
    Reconnected,
}

#[derive(Debug)]
pub struct Client {
    address: SocketAddr,
    peer: PeerId,
    admission: Admission,
    replay: Replay,
}

impl Client {
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub const fn peer(&self) -> PeerId {
        self.peer
    }

    pub const fn admission(&self) -> Admission {
        self.admission
    }

    pub(crate) fn replay(&self) -> &Replay {
        &self.replay
    }
}

pub(crate) fn accept(
    transport: &Transport,
    peers: &RwLock<PeerStore>,
    pin: Option<Pin>,
    network: &NetworkConfig,
) -> Result<Client, HostError> {
    let timeout = Duration::from_millis(u64::from(network.pairing_timeout_ms));
    let deadline = Instant::now() + timeout;
    let mut pairing = pin.map(|pin| PairingResponder::new(pin, network.pairing_max_attempts));
    let mut reconnect = ReconnectResponder::new();
    let mut buf = vec![0; usize::from(network.mtu)];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HostError::AcceptTimeout(timeout));
        }

        let Some((len, from)) = transport.recv_from_timeout(&mut buf, remaining)? else {
            continue;
        };
        let datagram = &buf[..len];
        let admitted = match PacketHeader::read(datagram).map(|header| header.stream) {
            Ok(StreamKind::Pairing) => match pairing.as_mut() {
                Some(responder) => {
                    refresh_peers(peers)?;
                    answer_pairing(transport, peers, responder, datagram, from)?
                }
                None => None,
            },
            Ok(StreamKind::Reconnect) => {
                refresh_peers(peers)?;
                answer_reconnect(transport, peers, &mut reconnect, datagram, from)?
            }
            _ => None,
        };

        if let Some(client) = admitted {
            return Ok(client);
        }
    }
}

pub(crate) fn refresh_peers(peers: &RwLock<PeerStore>) -> Result<(), HostError> {
    if peers.read().is_stale() {
        peers.write().refresh()?;
    }

    Ok(())
}

fn answer_pairing(
    transport: &Transport,
    peers: &RwLock<PeerStore>,
    responder: &mut PairingResponder,
    datagram: &[u8],
    from: SocketAddr,
) -> Result<Option<Client>, HostError> {
    let output = match responder.handle(datagram) {
        Ok(output) => output,
        Err(NetError::Protocol(_) | NetError::PairingKeyExchange(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    let Some(outcome) = output.paired else {
        if let Some(reply) = output.reply {
            transport.send_to(&reply, from)?;
        }
        if responder.is_locked() {
            return Err(NetError::PairingLocked.into());
        }
        return Ok(None);
    };

    let record = PeerRecord::new(&peer_name(outcome.client_name, from), outcome.secret).map_err(
        |source| HostError::PeerName {
            address: from,
            source,
        },
    )?;
    let peer = record.id();
    peers.write().remember(record)?;
    transport.install_session(&outcome.keys);

    let reply = output.reply.unwrap_or_default();
    transport.send_to(&reply, from)?;

    Ok(Some(Client {
        address: from,
        peer,
        admission: Admission::Paired,
        replay: Replay {
            request: datagram.to_vec(),
            reply,
        },
    }))
}

fn answer_reconnect(
    transport: &Transport,
    peers: &RwLock<PeerStore>,
    responder: &mut ReconnectResponder,
    datagram: &[u8],
    from: SocketAddr,
) -> Result<Option<Client>, HostError> {
    let output = match responder.handle(datagram, &peers.read()) {
        Ok(output) => output,
        Err(NetError::Protocol(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    let Some(accepted) = output.accepted else {
        if let Some(reply) = output.reply {
            transport.send_to(&reply, from)?;
        }
        return Ok(None);
    };

    transport.install_session(&accepted.keys);
    let reply = output.reply.unwrap_or_default();
    transport.send_to(&reply, from)?;

    Ok(Some(Client {
        address: from,
        peer: accepted.peer,
        admission: Admission::Reconnected,
        replay: Replay {
            request: datagram.to_vec(),
            reply,
        },
    }))
}

fn peer_name(announced: ClientName, address: SocketAddr) -> String {
    if announced.is_empty() {
        format!("client at {}", address.ip())
    } else {
        announced.as_str().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_final_request_is_answered_from_the_replay() {
        let replay = Replay {
            request: vec![1, 2, 3],
            reply: vec![9],
        };

        assert_eq!(replay.answer(&[1, 2, 3]), Some([9].as_slice()));
        assert_eq!(replay.answer(&[1, 2]), None);
    }

    #[test]
    fn peer_names_fit_the_store() {
        let longest = SocketAddr::new(
            "ffff:ffff:ffff:ffff:ffff:ffff:255.255.255.255"
                .parse()
                .unwrap(),
            1,
        );

        assert!(
            PeerRecord::new(
                &peer_name(ClientName::EMPTY, longest),
                rusplay_net::PeerSecret::from_bytes([1; 32])
            )
            .is_ok()
        );
    }

    #[test]
    fn an_announced_name_replaces_the_address_label() {
        let address = SocketAddr::new("192.168.1.9".parse().unwrap(), 1);
        let announced = ClientName::new("desk pc").unwrap();

        assert_eq!(peer_name(announced, address), "desk pc");
        assert_eq!(
            peer_name(ClientName::EMPTY, address),
            "client at 192.168.1.9"
        );
    }
}
