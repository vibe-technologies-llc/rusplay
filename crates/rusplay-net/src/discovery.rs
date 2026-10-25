use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusplay_config::NetworkConfig;
use rusplay_protocol::DiscoveryMessage;

use crate::NetError;

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHost {
    pub address: SocketAddr,
    pub name: String,
}

#[derive(Debug)]
pub struct DiscoveryResponder {
    socket: UdpSocket,
    port: u16,
    name: String,
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
            name: name.to_owned(),
        };
        announce.to_datagram()?;

        let socket = UdpSocket::bind(listen).map_err(|source| NetError::Bind {
            addr: listen,
            source,
        })?;

        if let Some(group) = group {
            socket
                .join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)
                .map_err(|source| NetError::MulticastJoin { group, source })?;
        }

        Ok(Self {
            socket,
            port: host_port,
            name: name.to_owned(),
        })
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
            name: self.name.clone(),
        }
        .to_datagram()?;

        self.socket
            .send_to(&reply, from)
            .map_err(|source| NetError::Send { peer: from, source })?;
        Ok(true)
    }
}

#[derive(Debug)]
pub struct DiscoveryBrowser {
    socket: UdpSocket,
    targets: Vec<SocketAddr>,
}

impl DiscoveryBrowser {
    pub fn new(targets: Vec<SocketAddr>) -> Result<Self, NetError> {
        let any = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let socket = UdpSocket::bind(any).map_err(|source| NetError::Bind { addr: any, source })?;

        socket.set_broadcast(true).map_err(NetError::Broadcast)?;
        socket
            .set_multicast_ttl_v4(MULTICAST_TTL)
            .map_err(NetError::MulticastTtl)?;

        Ok(Self { socket, targets })
    }

    pub fn with_options(options: DiscoveryOptions) -> Result<Self, NetError> {
        Self::new(options.targets())
    }

    pub fn scan(&self, timeout: Duration) -> Result<Vec<DiscoveredHost>, NetError> {
        let nonce = fresh_nonce(&self.socket);
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
                hosts.push(DiscoveredHost { address, name });
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

fn fresh_nonce(socket: &UdpSocket) -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    let port = socket.local_addr().map_or(0, |addr| u32::from(addr.port()));

    nanos ^ port.rotate_left(16)
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

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
            }]
        );
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
