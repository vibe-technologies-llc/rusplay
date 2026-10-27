use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use rusplay_config::{ConfigError, HostProfile, PeersConfig};
use rusplay_net::{MAX_PEER_NAME_BYTES, PeerRecord, PeerStore, write_owner_only};
use rusplay_protocol::PeerId;

use crate::ClientError;

const HEADER_PREFIX: &str = "rusplay-host-book ";
const FIRST_VERSION: u32 = 1;
const PROFILE_VERSION: u32 = 2;
const CURRENT_VERSION: u32 = PROFILE_VERSION;
const PROFILE_LINE_MARK: char = '\t';
const UNPAIRED: &str = "-";
const BOOK_EXTENSION: &str = "book";
const PEER_ID_HEX_LEN: usize = PeerId::LEN * 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedHost {
    pub name: String,
    pub address: SocketAddr,
    pub peer: Option<PeerRecord>,
    pub profile: HostProfile,
}

#[derive(Debug)]
pub struct HostBook {
    store: PeerStore,
    path: PathBuf,
    hosts: Vec<SavedHost>,
}

impl HostBook {
    pub fn open(peers: &PeersConfig) -> Result<Self, ClientError> {
        Self::from_store(PeerStore::open_client(peers)?)
    }

    pub fn from_store(store: PeerStore) -> Result<Self, ClientError> {
        let path = store.path().with_added_extension(BOOK_EXTENSION);

        let hosts = match fs::read_to_string(&path) {
            Ok(text) => parse(&text, &path, &store)?,
            Err(source) if source.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(ClientError::HostBookRead { path, source }),
        };

        Ok(Self { store, path, hosts })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn hosts(&self) -> &[SavedHost] {
        &self.hosts
    }

    pub fn save(&mut self, hosts: Vec<SavedHost>) -> Result<(), ClientError> {
        for peer in hosts.iter().filter_map(|host| host.peer.as_ref()) {
            if self.store.get(&peer.id()) != Some(peer) {
                self.store.remember(peer.clone())?;
            }
        }

        let text = render(&hosts).map_err(ClientError::HostProfileRender)?;

        write_owner_only(&self.path, &text).map_err(|source| ClientError::HostBookWrite {
            path: self.path.clone(),
            source,
        })?;

        let kept: Vec<PeerId> = hosts
            .iter()
            .filter_map(|host| host.peer.as_ref().map(PeerRecord::id))
            .collect();
        let stale: Vec<PeerId> = self
            .store
            .iter()
            .map(PeerRecord::id)
            .filter(|id| !kept.contains(id))
            .collect();
        for id in stale {
            self.store.forget(&id)?;
        }

        self.hosts = hosts;
        Ok(())
    }
}

fn render(hosts: &[SavedHost]) -> Result<String, ConfigError> {
    let mut text = format!("{HEADER_PREFIX}{CURRENT_VERSION}\n");

    for host in hosts {
        match &host.peer {
            Some(peer) => text.push_str(&peer.id().to_string()),
            None => text.push_str(UNPAIRED),
        }
        text.push(' ');
        text.push_str(&host.address.to_string());
        text.push(' ');
        text.push_str(&host.name);
        text.push('\n');

        for line in host.profile.to_toml_string()?.lines() {
            text.push(PROFILE_LINE_MARK);
            text.push_str(line);
            text.push('\n');
        }
    }

    Ok(text)
}

fn parse_version(header: Option<&str>) -> Option<u32> {
    header?
        .strip_prefix(HEADER_PREFIX)?
        .parse()
        .ok()
        .filter(|version| (FIRST_VERSION..=CURRENT_VERSION).contains(version))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_PEER_NAME_BYTES && !name.chars().any(char::is_control)
}

fn valid_peer_id(text: &str) -> bool {
    text == UNPAIRED
        || (text.len() == PEER_ID_HEX_LEN && text.chars().all(|c| c.is_ascii_hexdigit()))
}

struct PendingProfile {
    first_line: usize,
    text: String,
}

fn parse(text: &str, path: &Path, store: &PeerStore) -> Result<Vec<SavedHost>, ClientError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }

    let corrupt = |line: usize| ClientError::HostBookCorrupt {
        path: path.to_owned(),
        line,
    };

    let mut lines = text.lines();
    let Some(version) = parse_version(lines.next()) else {
        return Err(corrupt(1));
    };

    let mut hosts: Vec<SavedHost> = Vec::new();
    let mut paired_ids: Vec<&str> = Vec::new();
    let mut profile: Option<PendingProfile> = None;

    for (index, line) in lines.enumerate() {
        let line_number = index + 2;

        if let Some(profile_line) = line.strip_prefix(PROFILE_LINE_MARK) {
            if hosts.is_empty() || version < PROFILE_VERSION {
                return Err(corrupt(line_number));
            }
            let pending = profile.get_or_insert_with(|| PendingProfile {
                first_line: line_number,
                text: String::new(),
            });
            pending.text.push_str(profile_line);
            pending.text.push('\n');
            continue;
        }

        finish_profile(&mut hosts, profile.take(), &corrupt)?;

        let mut fields = line.splitn(3, ' ');
        let (Some(id), Some(address), Some(name)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(corrupt(line_number));
        };

        let address: SocketAddr = address.parse().map_err(|_| corrupt(line_number))?;
        let duplicate = hosts.iter().any(|host| host.address == address)
            || (id != UNPAIRED && paired_ids.contains(&id));
        if !valid_peer_id(id) || !valid_name(name) || duplicate {
            return Err(corrupt(line_number));
        }

        let peer = store
            .iter()
            .find(|record| record.id().to_string() == id)
            .cloned();
        if id != UNPAIRED {
            paired_ids.push(id);
        }

        hosts.push(SavedHost {
            name: name.to_owned(),
            address,
            peer,
            profile: HostProfile::default(),
        });
    }

    finish_profile(&mut hosts, profile.take(), &corrupt)?;

    Ok(hosts)
}

fn finish_profile(
    hosts: &mut [SavedHost],
    pending: Option<PendingProfile>,
    corrupt: &impl Fn(usize) -> ClientError,
) -> Result<(), ClientError> {
    let (Some(pending), Some(host)) = (pending, hosts.last_mut()) else {
        return Ok(());
    };

    host.profile =
        HostProfile::from_toml_str(&pending.text).map_err(|_| corrupt(pending.first_line))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use rusplay_config::{PresentMode, SoftwareDecoderConfig, VideoDecoderConfig};
    use rusplay_net::PeerSecret;

    use super::*;

    const HEADER_V1: &str = "rusplay-host-book 1";
    const HEADER_V2: &str = "rusplay-host-book 2";

    fn record(name: &str, byte: u8) -> PeerRecord {
        PeerRecord::new(name, PeerSecret::from_bytes([byte; 32])).unwrap()
    }

    fn host(name: &str, address: &str, peer: Option<PeerRecord>) -> SavedHost {
        SavedHost {
            name: name.to_owned(),
            address: address.parse().unwrap(),
            peer,
            profile: HostProfile::default(),
        }
    }

    fn open(directory: &Path) -> HostBook {
        HostBook::from_store(PeerStore::open(directory.join("paired-hosts")).unwrap()).unwrap()
    }

    #[test]
    fn a_missing_book_is_empty() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(open(directory.path()).hosts(), []);
    }

    #[test]
    fn hosts_and_pairings_survive_a_reload() {
        let directory = tempfile::tempdir().unwrap();
        let mut book = open(directory.path());
        let hosts = vec![
            host("Living room pc", "10.0.0.2:47800", Some(record("desk", 1))),
            host("v6 box", "[fe80::1]:5000", None),
        ];

        book.save(hosts.clone()).unwrap();
        let reloaded = open(directory.path());

        assert_eq!(reloaded.hosts(), hosts);
        assert_eq!(book.path(), directory.path().join("paired-hosts.book"));
    }

    #[test]
    fn forgetting_a_pairing_drops_the_secret_from_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let mut book = open(directory.path());
        book.save(vec![host("a", "10.0.0.2:47800", Some(record("a", 1)))])
            .unwrap();

        book.save(vec![host("a", "10.0.0.2:47800", None)]).unwrap();
        let reloaded = open(directory.path());

        assert_eq!(reloaded.hosts()[0].peer, None);
        assert!(reloaded.store.is_empty());
    }

    #[test]
    fn removing_a_host_drops_its_pairing_but_keeps_the_others() {
        let directory = tempfile::tempdir().unwrap();
        let mut book = open(directory.path());
        book.save(vec![
            host("a", "10.0.0.2:47800", Some(record("a", 1))),
            host("b", "10.0.0.3:47800", Some(record("b", 2))),
        ])
        .unwrap();

        book.save(vec![host("b", "10.0.0.3:47800", Some(record("b", 2)))])
            .unwrap();
        let reloaded = open(directory.path());

        assert_eq!(reloaded.hosts().len(), 1);
        assert_eq!(reloaded.store.len(), 1);
    }

    #[test]
    fn a_pairing_missing_from_the_store_loads_unpaired() {
        let directory = tempfile::tempdir().unwrap();
        let mut book = open(directory.path());
        book.save(vec![host("a", "10.0.0.2:47800", Some(record("a", 1)))])
            .unwrap();
        fs::remove_file(directory.path().join("paired-hosts")).unwrap();

        let reloaded = open(directory.path());

        assert_eq!(reloaded.hosts().len(), 1);
        assert_eq!(reloaded.hosts()[0].peer, None);
    }

    #[test]
    fn the_book_is_written_owner_only() {
        let directory = tempfile::tempdir().unwrap();
        let mut book = open(directory.path());

        book.save(vec![host("a", "10.0.0.2:47800", None)]).unwrap();

        let mode = fs::metadata(book.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn corrupt_books_are_refused_with_the_line_number() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paired-hosts.book");
        let id = record("a", 1).id().to_string();
        let cases = [
            ("no header\n".to_owned(), 1),
            (format!("{HEADER_V2}\ngarbage\n"), 2),
            (format!("{HEADER_V2}\n- not-an-address name\n"), 2),
            (format!("{HEADER_V2}\n- 10.0.0.2:1 \n"), 2),
            (format!("{HEADER_V2}\nxyz 10.0.0.2:1 name\n"), 2),
            (format!("{HEADER_V2}\n- 10.0.0.2:1 a\n- 10.0.0.2:1 b\n"), 3),
            (
                format!("{HEADER_V2}\n{id} 10.0.0.2:1 a\n{id} 10.0.0.3:1 b\n"),
                3,
            ),
            (format!("{HEADER_V2}\n- 10.0.0.2:1 bad\u{7}name\n"), 2),
        ];

        for (text, line) in cases {
            fs::write(&path, &text).unwrap();

            let result = HostBook::from_store(
                PeerStore::open(directory.path().join("paired-hosts")).unwrap(),
            );

            assert!(
                matches!(result, Err(ClientError::HostBookCorrupt { line: found, .. }) if found == line),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_failed_write_leaves_the_book_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let mut book = open(directory.path());
        fs::create_dir(directory.path().join("paired-hosts.book.new")).unwrap();

        let result = book.save(vec![host("a", "10.0.0.2:47800", None)]);

        assert!(matches!(result, Err(ClientError::HostBookWrite { .. })));
        assert_eq!(book.hosts(), []);
    }

    fn profiled(address: &str) -> SavedHost {
        SavedHost {
            profile: HostProfile {
                decoder: Some(VideoDecoderConfig::Software(
                    SoftwareDecoderConfig::default(),
                )),
                present_mode: Some(PresentMode::Fifo),
                keyboard: Some(false),
                ..HostProfile::default()
            },
            ..host("Den", address, None)
        }
    }

    #[test]
    fn profiles_survive_a_reload_next_to_hosts_without_one() {
        let directory = tempfile::tempdir().unwrap();
        let mut book = open(directory.path());
        let hosts = vec![
            host("plain", "10.0.0.2:47800", Some(record("plain", 1))),
            profiled("10.0.0.3:47800"),
            host("also plain", "10.0.0.4:47800", None),
        ];

        book.save(hosts.clone()).unwrap();
        let reloaded = open(directory.path());

        assert_eq!(reloaded.hosts(), hosts);
        assert!(reloaded.hosts()[0].profile.is_empty());
        assert!(!reloaded.hosts()[1].profile.is_empty());
    }

    #[test]
    fn a_version_one_book_loads_without_profiles_and_is_saved_as_version_two() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paired-hosts.book");
        fs::write(
            &path,
            format!("{HEADER_V1}\n- 10.0.0.2:47800 old\n- 10.0.0.3:47800 older\n"),
        )
        .unwrap();

        let mut book = open(directory.path());
        let loaded = book.hosts().to_vec();
        book.save(loaded.clone()).unwrap();

        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().all(|host| host.profile.is_empty()));
        assert!(fs::read_to_string(&path).unwrap().starts_with(HEADER_V2));
        assert_eq!(open(directory.path()).hosts(), loaded);
    }

    #[test]
    fn clearing_a_profile_removes_its_lines() {
        let directory = tempfile::tempdir().unwrap();
        let mut book = open(directory.path());
        book.save(vec![profiled("10.0.0.3:47800")]).unwrap();

        book.save(vec![host("Den", "10.0.0.3:47800", None)])
            .unwrap();

        let text = fs::read_to_string(book.path()).unwrap();
        assert_eq!(text, format!("{HEADER_V2}\n- 10.0.0.3:47800 Den\n"));
    }

    #[test]
    fn corrupt_profiles_are_refused_with_the_line_number() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paired-hosts.book");
        let cases = [
            (format!("{HEADER_V2}\n\tpresent_mode = \"mailbox\"\n"), 2),
            (
                format!("{HEADER_V2}\n- 10.0.0.2:1 a\n\tpresent_mode = \"nope\"\n"),
                3,
            ),
            (
                format!("{HEADER_V2}\n- 10.0.0.2:1 a\n\tbitrate = 3\n- 10.0.0.3:1 b\n"),
                3,
            ),
            (
                format!("{HEADER_V1}\n- 10.0.0.2:1 a\n\tpresent_mode = \"fifo\"\n"),
                3,
            ),
            ("rusplay-host-book 3\n".to_owned(), 1),
        ];

        for (text, line) in cases {
            fs::write(&path, &text).unwrap();

            let result = HostBook::from_store(
                PeerStore::open(directory.path().join("paired-hosts")).unwrap(),
            );

            assert!(
                matches!(result, Err(ClientError::HostBookCorrupt { line: found, .. }) if found == line),
                "{text:?}"
            );
        }
    }
}
