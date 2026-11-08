use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use if_addrs::{IfAddr, Interface};
use rusplay_config::NetworkConfig;
use rusplay_protocol::{DiscoveryMessage, HostId};

use crate::{NetError, pairing::fill_random};

const MULTICAST_TTL: u32 = 1;
const SCAN_RESEND_DIVISIONS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryOptions {
    pub port: u16,
    pub group: Ipv4Addr,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self::from(&NetworkConfig::default())
    }
}

impl From<&NetworkConfig> for DiscoveryOptions {
    fn from(config: &NetworkConfig) -> Self {
        Self {
            port: config.discovery_port,
            group: config.discovery_group,
        }
    }
}

impl DiscoveryOptions {
    pub fn targets(&self) -> Vec<SocketAddr> {
        vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), self.port),
            SocketAddr::new(IpAddr::V4(self.group), self.port),
        ]
    }

    pub fn link_targets(&self, links: &[Ipv4Link]) -> Vec<SocketAddr> {
        let mut targets = self.targets();
        for broadcast in links.iter().filter_map(|link| link.broadcast) {
            let target = SocketAddr::new(IpAddr::V4(broadcast), self.port);
            if !targets.contains(&target) {
                targets.push(target);
            }
        }

        targets
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Link {
    pub address: Ipv4Addr,
    pub broadcast: Option<Ipv4Addr>,
}

impl Ipv4Link {
    fn of(interface: &Interface) -> Option<Self> {
        match &interface.addr {
            IfAddr::V4(v4) if interface.is_oper_up() && !v4.ip.is_loopback() => Some(Self {
                address: v4.ip,
                broadcast: v4.broadcast.filter(|broadcast| !broadcast.is_broadcast()),
            }),
            _ => None,
        }
    }
}

pub fn ipv4_links() -> Vec<Ipv4Link> {
    let mut links: Vec<Ipv4Link> = Vec::new();
    for link in if_addrs::get_if_addrs()
        .unwrap_or_default()
        .iter()
        .filter_map(Ipv4Link::of)
    {
        if !links.contains(&link) {
            links.push(link);
        }
    }

    links
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHost {
    pub address: SocketAddr,
    pub name: String,
    pub busy: bool,
    pub id: HostId,
}

#[derive(Debug)]
pub struct DiscoveryResponder {
    socket: UdpSocket,
    port: u16,
    name: String,
    busy: Arc<AtomicBool>,
    host_id: HostId,
}

impl DiscoveryResponder {
    pub fn bind(
        listen: SocketAddr,
        group: Option<Ipv4Addr>,
        host_port: u16,
        name: &str,
    ) -> Result<Self, NetError> {
        let announce = DiscoveryMessage::Announce {
            nonce: 0,
            port: host_port,
            busy: false,
            host_id: HostId::NONE,
            name: name.to_owned(),
        };
        announce.to_datagram()?;

        let socket = UdpSocket::bind(listen).map_err(|source| NetError::Bind {
            addr: listen,
            source,
        })?;

        if let Some(group) = group {
            join_on_every_link(&socket, group, &ipv4_links())?;
        }

        Ok(Self {
            socket,
            port: host_port,
            name: name.to_owned(),
            busy: Arc::new(AtomicBool::new(false)),
            host_id: HostId::NONE,
        })
    }

    #[must_use]
    pub const fn with_host_id(mut self, host_id: HostId) -> Self {
        self.host_id = host_id;
        self
    }

    #[must_use]
    pub fn with_busy_flag(mut self, busy: Arc<AtomicBool>) -> Self {
        self.busy = busy;
        self
    }

    pub fn bind_with_options(
        options: DiscoveryOptions,
        host_port: u16,
        name: &str,
    ) -> Result<Self, NetError> {
        let listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), options.port);

        Self::bind(listen, Some(options.group), host_port, name)
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.socket.local_addr().map_err(NetError::LocalAddr)
    }

    pub fn handle_next(&self, timeout: Duration) -> Result<bool, NetError> {
        self.socket
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
            .map_err(NetError::ReadTimeout)?;

        let mut buf = [0; DiscoveryMessage::MAX_ENCODED_LEN + 16];
        let (len, from) = match self.socket.recv_from(&mut buf) {
            Ok(received) => received,
            Err(error) if is_timeout(&error) => return Ok(false),
            Err(error) => return Err(NetError::Receive(error)),
        };

        let Ok(DiscoveryMessage::Query { nonce }) = DiscoveryMessage::from_datagram(&buf[..len])
        else {
            return Ok(false);
        };

        let reply = DiscoveryMessage::Announce {
            nonce,
            port: self.port,
            busy: self.busy.load(Ordering::Relaxed),
            host_id: self.host_id,
            name: self.name.clone(),
        }
        .to_datagram()?;

        self.socket
            .send_to(&reply, from)
            .map_err(|source| NetError::Send { peer: from, source })?;
        Ok(true)
    }
}

fn join_on_every_link(
    socket: &UdpSocket,
    group: Ipv4Addr,
    links: &[Ipv4Link],
) -> Result<(), NetError> {
    let mut joined = false;
    for link in links {
        joined |= socket.join_multicast_v4(&group, &link.address).is_ok();
    }
    if joined {
        return Ok(());
    }

    socket
        .join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)
        .map_err(|source| NetError::MulticastJoin { group, source })
}

#[derive(Debug)]
pub struct DiscoveryBrowser {
    socket: UdpSocket,
    targets: Vec<SocketAddr>,
    links: Vec<Ipv4Link>,
}

impl DiscoveryBrowser {
    pub fn new(targets: Vec<SocketAddr>) -> Result<Self, NetError> {
        let any = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let socket = UdpSocket::bind(any).map_err(|source| NetError::Bind { addr: any, source })?;

        socket.set_broadcast(true).map_err(NetError::Broadcast)?;
        socket
            .set_multicast_ttl_v4(MULTICAST_TTL)
            .map_err(NetError::MulticastTtl)?;

        Ok(Self {
            socket,
            targets,
            links: Vec::new(),
        })
    }

    pub fn with_options(options: DiscoveryOptions) -> Result<Self, NetError> {
        let links = ipv4_links();
        let mut browser = Self::new(options.link_targets(&links))?;
        browser.links = links;

        Ok(browser)
    }

    pub fn scan(&self, timeout: Duration) -> Result<Vec<DiscoveredHost>, NetError> {
        let nonce = fresh_nonce()?;
        let query = DiscoveryMessage::Query { nonce }.to_datagram()?;
        let start = Instant::now();
        let deadline = start + timeout;
        let resend_every = (timeout / SCAN_RESEND_DIVISIONS).max(Duration::from_millis(1));

        let mut hosts: Vec<DiscoveredHost> = Vec::new();
        let mut next_send = start;
        let mut buf = [0; DiscoveryMessage::MAX_ENCODED_LEN + 16];

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(hosts);
            }

            if now >= next_send {
                self.send_query(&query)?;
                next_send = now + resend_every;
            }

            let wait = next_send.min(deadline).saturating_duration_since(now);
            self.socket
                .set_read_timeout(Some(wait.max(Duration::from_millis(1))))
                .map_err(NetError::ReadTimeout)?;

            let (len, from) = match self.socket.recv_from(&mut buf) {
                Ok(received) => received,
                Err(error) if is_timeout(&error) => continue,
                Err(error) => return Err(NetError::Receive(error)),
            };

            let Ok(DiscoveryMessage::Announce {
                nonce: echoed,
                port,
                busy,
                host_id,
                name,
            }) = DiscoveryMessage::from_datagram(&buf[..len])
            else {
                continue;
            };

            if echoed != nonce {
                continue;
            }

            let address = SocketAddr::new(from.ip(), port);
            if !hosts.iter().any(|host| host.address == address) {
                hosts.push(DiscoveredHost {
                    address,
                    name,
                    busy,
                    id: host_id,
                });
            }
        }
    }

    fn send_query(&self, query: &[u8]) -> Result<(), NetError> {
        let mut last_error = None;
        let mut delivered = false;

        for target in &self.targets {
            match self.socket.send_to(query, target) {
                Ok(_) => delivered = true,
                Err(source) => last_error = Some((*target, source)),
            }
        }

        let groups = self
            .targets
            .iter()
            .filter(|target| target.ip().is_multicast());
        for group in groups {
            for link in &self.links {
                let sent = rustix::net::sockopt::set_ip_multicast_if(&self.socket, &link.address)
                    .map_err(std::io::Error::from)
                    .and_then(|()| self.socket.send_to(query, group));
                match sent {
                    Ok(_) => delivered = true,
                    Err(source) => last_error = Some((*group, source)),
                }
            }
        }
        if !self.links.is_empty() {
            rustix::net::sockopt::set_ip_multicast_if(&self.socket, &Ipv4Addr::UNSPECIFIED).ok();
        }

        match last_error {
            Some((peer, source)) if !delivered => Err(NetError::Send { peer, source }),
            _ => Ok(()),
        }
    }
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn fresh_nonce() -> Result<u32, NetError> {
    let mut bytes = [0; 4];
    fill_random(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use rusplay_protocol::ProtocolError;

    use super::*;

    const LOOPBACK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    const SCAN: Duration = Duration::from_millis(600);

    fn responder(name: &str, host_port: u16) -> Arc<DiscoveryResponder> {
        Arc::new(DiscoveryResponder::bind(LOOPBACK, None, host_port, name).unwrap())
    }

    fn serve(responder: &Arc<DiscoveryResponder>, answers: usize) -> thread::JoinHandle<()> {
        let responder = Arc::clone(responder);
        thread::spawn(move || {
            let mut answered = 0;
            while answered < answers {
                if responder.handle_next(Duration::from_secs(5)).unwrap() {
                    answered += 1;
                }
            }
        })
    }

    #[test]
    fn browser_finds_a_responding_host() {
        let host = responder("couch-pc", 47_800);
        let server = serve(&host, 1);
        let browser = DiscoveryBrowser::new(vec![host.local_addr().unwrap()]).unwrap();

        let found = browser.scan(SCAN).unwrap();

        server.join().unwrap();
        assert_eq!(
            found,
            [DiscoveredHost {
                address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 47_800),
                name: "couch-pc".to_owned(),
                busy: false,
                id: HostId::NONE,
            }]
        );
    }

    #[test]
    fn the_announce_follows_the_responders_busy_flag() {
        let busy = Arc::new(AtomicBool::new(true));
        let idle = responder("idle", 1111);
        let mid_session = Arc::new(
            DiscoveryResponder::bind(LOOPBACK, None, 2222, "mid-session")
                .unwrap()
                .with_busy_flag(Arc::clone(&busy)),
        );
        let servers = [serve(&idle, 1), serve(&mid_session, 1)];
        let browser = DiscoveryBrowser::new(vec![
            idle.local_addr().unwrap(),
            mid_session.local_addr().unwrap(),
        ])
        .unwrap();

        let mut found = browser.scan(SCAN).unwrap();

        for server in servers {
            server.join().unwrap();
        }
        found.sort_by_key(|host| host.address.port());
        let states: Vec<(&str, bool)> = found
            .iter()
            .map(|host| (host.name.as_str(), host.busy))
            .collect();
        assert_eq!(states, [("idle", false), ("mid-session", true)]);
    }

    #[test]
    fn the_announce_carries_the_responders_host_id() {
        let id = HostId([7; HostId::LEN]);
        let host = Arc::new(
            DiscoveryResponder::bind(LOOPBACK, None, 47_800, "couch-pc")
                .unwrap()
                .with_host_id(id),
        );
        let server = serve(&host, 1);
        let browser = DiscoveryBrowser::new(vec![host.local_addr().unwrap()]).unwrap();

        let found = browser.scan(SCAN).unwrap();

        server.join().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, id);
    }

    #[test]
    fn browser_collects_every_host_once() {
        let first = responder("one", 1111);
        let second = responder("two", 2222);
        let servers = [serve(&first, 1), serve(&second, 1)];
        let browser = DiscoveryBrowser::new(vec![
            first.local_addr().unwrap(),
            second.local_addr().unwrap(),
        ])
        .unwrap();

        let mut found = browser.scan(SCAN).unwrap();

        for server in servers {
            server.join().unwrap();
        }
        found.sort_by_key(|host| host.address.port());
        let names: Vec<&str> = found.iter().map(|host| host.name.as_str()).collect();
        assert_eq!(names, ["one", "two"]);
    }

    #[test]
    fn scan_with_nobody_listening_returns_empty_after_the_timeout() {
        let silent = UdpSocket::bind(LOOPBACK).unwrap();
        let browser = DiscoveryBrowser::new(vec![silent.local_addr().unwrap()]).unwrap();
        let start = Instant::now();

        let found = browser.scan(Duration::from_millis(150)).unwrap();

        assert_eq!(found.len(), 0);
        assert!(start.elapsed() >= Duration::from_millis(150));
    }

    #[test]
    fn responder_ignores_garbage() {
        let host = responder("quiet", 1);
        let prober = UdpSocket::bind(LOOPBACK).unwrap();
        prober
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();

        prober
            .send_to(&[1, 2, 3], host.local_addr().unwrap())
            .unwrap();
        let answered = host.handle_next(Duration::from_millis(500)).unwrap();

        assert!(!answered);
        assert!(prober.recv_from(&mut [0; 64]).is_err());
    }

    #[test]
    fn unsolicited_announcement_is_ignored() {
        let intruder = UdpSocket::bind(LOOPBACK).unwrap();
        let browser = DiscoveryBrowser::new(vec![intruder.local_addr().unwrap()]).unwrap();
        let browser_addr = browser.socket.local_addr().unwrap();
        let forged = DiscoveryMessage::Announce {
            nonce: 12345,
            port: 9,
            busy: false,
            host_id: HostId::NONE,
            name: "evil".to_owned(),
        }
        .to_datagram()
        .unwrap();
        intruder.send_to(&forged, browser_addr).unwrap();

        let found = browser.scan(Duration::from_millis(100)).unwrap();

        assert_eq!(found.len(), 0);
    }

    #[test]
    fn oversized_host_name_is_refused_at_bind() {
        let name = "n".repeat(DiscoveryMessage::MAX_NAME_LEN + 1);

        assert!(matches!(
            DiscoveryResponder::bind(LOOPBACK, None, 1, &name),
            Err(NetError::Protocol(_))
        ));
    }

    #[test]
    fn nonces_are_drawn_from_the_random_source() {
        let nonces: std::collections::HashSet<u32> =
            (0..32).map(|_| fresh_nonce().unwrap()).collect();

        assert!(nonces.len() > 30);
    }

    #[test]
    fn host_name_with_a_control_character_is_refused_at_bind() {
        assert!(matches!(
            DiscoveryResponder::bind(LOOPBACK, None, 1, "host\u{7}name"),
            Err(NetError::Protocol(
                ProtocolError::DiscoveryNameControlCharacter
            ))
        ));
    }

    #[test]
    fn each_link_adds_its_directed_broadcast_once() {
        let options = DiscoveryOptions::default();
        let links = [
            Ipv4Link {
                address: Ipv4Addr::new(192, 168, 1, 20),
                broadcast: Some(Ipv4Addr::new(192, 168, 1, 255)),
            },
            Ipv4Link {
                address: Ipv4Addr::new(10, 0, 7, 3),
                broadcast: Some(Ipv4Addr::new(10, 0, 7, 255)),
            },
            Ipv4Link {
                address: Ipv4Addr::new(192, 168, 1, 21),
                broadcast: Some(Ipv4Addr::new(192, 168, 1, 255)),
            },
            Ipv4Link {
                address: Ipv4Addr::new(100, 64, 0, 9),
                broadcast: None,
            },
        ];

        let targets = options.link_targets(&links);

        let port = options.port;
        assert_eq!(&targets[..2], options.targets().as_slice());
        assert_eq!(
            &targets[2..],
            [
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 255)), port),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 7, 255)), port),
            ]
        );
        assert_eq!(options.link_targets(&[]), options.targets());
    }

    #[test]
    fn the_links_leave_out_loopback_and_repeat_no_address() {
        let links = ipv4_links();

        assert!(links.iter().all(|link| !link.address.is_loopback()));
        for (index, link) in links.iter().enumerate() {
            assert!(!links[index + 1..].contains(link));
        }
    }

    #[test]
    fn default_options_target_broadcast_and_the_group() {
        let options = DiscoveryOptions::default();

        assert_eq!(
            options.targets(),
            [
                SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), 47_801),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 47, 80)), 47_801),
            ]
        );
    }

    #[test]
    fn options_follow_the_network_config() {
        let config = NetworkConfig {
            discovery_port: 5555,
            discovery_group: Ipv4Addr::new(239, 9, 9, 9),
            ..NetworkConfig::default()
        };

        assert_eq!(
            DiscoveryOptions::from(&config),
            DiscoveryOptions {
                port: 5555,
                group: Ipv4Addr::new(239, 9, 9, 9),
            }
        );
    }
}
