use std::{net::SocketAddr, sync::Arc};

use parking_lot::RwLock;
use rusplay_config::{Config, ConfigEnvironment, ConfigError, ConfigRole};
pub use rusplay_host::{Admission, HostEvent, HostStats, NegotiatedFormat, PeerId, Pin};
use rusplay_host::{HostError, HostService, NetError, PeerStore, StreamSnapshot};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostingError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Peers(#[from] NetError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedClient {
    pub id: PeerId,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveStream {
    pub client: SocketAddr,
    pub stats: HostStats,
    pub format: Option<NegotiatedFormat>,
}

impl From<StreamSnapshot> for LiveStream {
    fn from(snapshot: StreamSnapshot) -> Self {
        Self {
            client: snapshot.client,
            stats: snapshot.stats,
            format: snapshot.format,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PairedClients {
    store: Arc<RwLock<PeerStore>>,
}

impl PairedClients {
    pub fn open(config: &Config) -> Result<Self, HostingError> {
        let store = PeerStore::open_host(&config.peers)?;

        Ok(Self {
            store: Arc::new(RwLock::new(store)),
        })
    }

    pub fn exposure(&self) -> Option<String> {
        let store = self.store.read();

        store.exposure().map(|exposure| {
            format!(
                "The paired client file {} {exposure}",
                store.path().display()
            )
        })
    }

    pub fn list(&self) -> Result<Vec<PairedClient>, HostingError> {
        self.refresh()?;

        Ok(self
            .store
            .read()
            .iter()
            .map(|record| PairedClient {
                id: record.id(),
                name: record.name().to_owned(),
            })
            .collect())
    }

    pub fn revoke(&self, id: &PeerId) -> Result<bool, HostingError> {
        self.refresh()?;

        Ok(self.store.write().forget(id)?)
    }

    fn refresh(&self) -> Result<(), HostingError> {
        if self.store.read().is_stale() {
            self.store.write().refresh()?;
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct HostHandle {
    service: HostService,
}

impl HostHandle {
    pub const fn address(&self) -> SocketAddr {
        self.service.address()
    }

    pub fn poll_events(&self) -> Vec<HostEvent> {
        self.service.poll_events()
    }

    pub fn stream(&self) -> Option<LiveStream> {
        self.service.snapshot().map(LiveStream::from)
    }

    pub fn renew_pin(&self) {
        self.service.renew_pin();
    }

    pub fn is_finished(&self) -> bool {
        self.service.is_finished()
    }

    pub fn stop(self) -> Vec<HostEvent> {
        self.service.stop()
    }
}

pub fn load_host_config(environment: &ConfigEnvironment) -> Result<Config, ConfigError> {
    let path = environment.config_path(ConfigRole::Host)?;

    Config::load_or_default(&path)
}

pub fn start(environment: &ConfigEnvironment) -> Result<(HostHandle, PairedClients), HostingError> {
    let config = load_host_config(environment)?;
    let clients = PairedClients::open(&config)?;
    let service = HostService::start(config, &clients.store)?;

    Ok((HostHandle { service }, clients))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use rusplay_config::PeersConfig;
    use rusplay_net::{PeerRecord, PeerSecret};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rusplay-app-hosting-{}-{name}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();

        root
    }

    fn config_with_store(root: &std::path::Path) -> Config {
        Config {
            peers: PeersConfig {
                host_file: Some(root.join("paired-clients")),
                ..PeersConfig::default()
            },
            ..Config::default()
        }
    }

    fn remember(clients: &PairedClients, name: &str, byte: u8) -> PeerId {
        let record = PeerRecord::new(name, PeerSecret::from_bytes([byte; 32])).unwrap();
        let id = record.id();
        clients.store.write().remember(record).unwrap();

        id
    }

    #[test]
    fn an_unwritten_store_lists_no_clients() {
        let root = scratch("empty");
        let clients = PairedClients::open(&config_with_store(&root)).unwrap();

        assert_eq!(clients.list().unwrap(), Vec::new());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_remembered_client_is_listed_by_name_and_id() {
        let root = scratch("listed");
        let clients = PairedClients::open(&config_with_store(&root)).unwrap();

        let id = remember(&clients, "laptop", 1);

        assert_eq!(
            clients.list().unwrap(),
            vec![PairedClient {
                id,
                name: "laptop".to_owned()
            }]
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn revoking_removes_only_that_client_and_reports_whether_it_existed() {
        let root = scratch("revoke");
        let clients = PairedClients::open(&config_with_store(&root)).unwrap();
        let first = remember(&clients, "laptop", 1);
        let second = remember(&clients, "desk pc", 2);

        assert!(clients.revoke(&first).unwrap());
        assert!(!clients.revoke(&first).unwrap());

        let names: Vec<_> = clients
            .list()
            .unwrap()
            .into_iter()
            .map(|client| client.id)
            .collect();
        assert_eq!(names, vec![second]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_pairing_written_by_another_handle_shows_up_after_the_file_changes() {
        let root = scratch("shared");
        let config = config_with_store(&root);
        let viewer = PairedClients::open(&config).unwrap();
        let writer = PairedClients::open(&config).unwrap();

        let id = remember(&writer, "phone", 3);

        assert_eq!(
            viewer.list().unwrap().first().map(|client| client.id),
            Some(id)
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_host_config_comes_from_host_toml_and_not_client_toml() {
        let root = scratch("config");
        fs::create_dir_all(root.join("rusplay")).unwrap();
        fs::write(root.join("rusplay/client.toml"), "[network]\nport = 4242\n").unwrap();
        fs::write(root.join("rusplay/host.toml"), "[network]\nport = 5151\n").unwrap();
        let environment = ConfigEnvironment {
            xdg_config_home: Some(root.clone()),
            home: None,
        };

        let config = load_host_config(&environment).unwrap();

        assert_eq!(config.network.port, 5151);
        fs::remove_dir_all(root).ok();
    }
}
