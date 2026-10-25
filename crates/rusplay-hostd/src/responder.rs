use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rusplay_config::NetworkConfig;
use rusplay_host::{DiscoveryResponder, NetError};
use thiserror::Error;

const HOSTNAME_FILE: &str = "/proc/sys/kernel/hostname";
const FALLBACK_NAME: &str = "rusplay";
const MAX_NAME_BYTES: usize = 64;
const QUERY_WAIT: Duration = Duration::from_millis(200);
const RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum ResponderError {
    #[error("failed to bind the discovery responder")]
    Bind(#[source] NetError),
    #[error("failed to spawn the discovery thread")]
    Spawn(#[source] std::io::Error),
}

pub struct Responder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Responder {
    pub fn start(
        network: &NetworkConfig,
        host_port: u16,
        configured_name: Option<&str>,
    ) -> Result<Self, ResponderError> {
        let name = announced_name(configured_name, system_hostname());
        let responder = DiscoveryResponder::bind_with_options(network.into(), host_port, &name)
            .map_err(ResponderError::Bind)?;

        Self::spawn(responder)
    }

    pub fn spawn(responder: DiscoveryResponder) -> Result<Self, ResponderError> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("rusplay-hostd-discovery".to_owned())
                .spawn(move || answer(&responder, &stop))
                .map_err(ResponderError::Spawn)?
        };

        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn answer(responder: &DiscoveryResponder, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        if let Err(error) = responder.handle_next(QUERY_WAIT) {
            eprintln!("rusplay-hostd: discovery: {error}");
            thread::sleep(RETRY_DELAY);
        }
    }
}

fn system_hostname() -> Option<String> {
    fs::read_to_string(HOSTNAME_FILE).ok()
}

pub fn announced_name(configured: Option<&str>, hostname: Option<String>) -> String {
    let chosen = configured
        .map(str::to_owned)
        .or(hostname)
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| FALLBACK_NAME.to_owned());

    truncate_at_boundary(chosen, MAX_NAME_BYTES)
}

fn truncate_at_boundary(mut name: String, max_bytes: usize) -> String {
    let mut end = name.len().min(max_bytes);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name.truncate(end);

    name
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use rusplay_net::DiscoveryBrowser;

    use super::*;

    #[test]
    fn the_configured_name_wins_over_the_hostname() {
        let name = announced_name(Some("living room"), Some("machine\n".to_owned()));

        assert_eq!(name, "living room");
    }

    #[test]
    fn the_hostname_is_trimmed() {
        assert_eq!(
            announced_name(None, Some("machine\n".to_owned())),
            "machine"
        );
    }

    #[test]
    fn a_missing_or_blank_hostname_falls_back_to_the_program_name() {
        assert_eq!(announced_name(None, None), FALLBACK_NAME);
        assert_eq!(announced_name(None, Some(" \n".to_owned())), FALLBACK_NAME);
    }

    #[test]
    fn a_long_name_is_cut_on_a_character_boundary() {
        let long = "é".repeat(40);

        let name = announced_name(Some(&long), None);

        assert_eq!(name.len(), MAX_NAME_BYTES);
        assert_eq!(name, "é".repeat(32));
    }

    #[test]
    fn a_browser_finds_the_running_responder() {
        let listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let responder = DiscoveryResponder::bind(listen, None, 47_800, "test host").unwrap();
        let target = responder.local_addr().unwrap();
        let running = Responder::spawn(responder).unwrap();

        let found = DiscoveryBrowser::new(vec![target])
            .unwrap()
            .scan(Duration::from_secs(2))
            .unwrap();
        drop(running);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "test host");
        assert_eq!(found[0].address.port(), 47_800);
    }
}
