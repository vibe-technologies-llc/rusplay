use std::{
    env,
    ffi::OsString,
    fmt, fs,
    io::{self},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::SystemTime,
};

use rusplay_config::PeersConfig;
use rusplay_protocol::PeerId;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::NetError;

pub const PEER_SECRET_LEN: usize = 32;
pub const MAX_PEER_NAME_BYTES: usize = 64;
pub const MAX_PEERS: usize = 256;

const APP_DIR: &str = "rusplay";
const HOST_FILE: &str = "paired-clients";
const CLIENT_FILE: &str = "paired-hosts";
#[cfg(test)]
const STAGING_SUFFIX: &str = "new";
#[cfg(test)]
const OWNER_ONLY_FILE: u32 = 0o600;
const OWNER_ONLY_DIRECTORY: u32 = 0o700;
const GROUP_AND_OTHER_BITS: u32 = 0o077;
const PERMISSION_BITS: u32 = 0o777;
const ID_DOMAIN: &[u8] = b"rusplay peer id";

#[derive(Clone)]
pub struct PeerSecret([u8; PEER_SECRET_LEN]);

impl PeerSecret {
    pub const fn from_bytes(bytes: [u8; PEER_SECRET_LEN]) -> Self {
        Self(bytes)
    }

    pub fn id(&self) -> PeerId {
        let digest = Sha256::new()
            .chain_update(ID_DOMAIN)
            .chain_update(self.0)
            .finalize();

        let mut id = [0; PeerId::LEN];
        id.copy_from_slice(&digest[..PeerId::LEN]);
        PeerId(id)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; PEER_SECRET_LEN] {
        &self.0
    }
}

impl PartialEq for PeerSecret {
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for PeerSecret {}

impl Drop for PeerSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for PeerSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("PeerSecret").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRecord {
    id: PeerId,
    name: String,
    secret: PeerSecret,
}

impl PeerRecord {
    pub fn new(name: &str, secret: PeerSecret) -> Result<Self, NetError> {
        if name.is_empty() {
            return Err(NetError::PeerNameEmpty);
        }

        if name.len() > MAX_PEER_NAME_BYTES {
            return Err(NetError::PeerNameTooLong {
                len: name.len(),
                max: MAX_PEER_NAME_BYTES,
            });
        }

        if name.chars().any(char::is_control) {
            return Err(NetError::PeerNameControlCharacter);
        }

        Ok(Self {
            id: secret.id(),
            name: name.to_owned(),
            secret,
        })
    }

    pub const fn id(&self) -> PeerId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn secret(&self) -> &PeerSecret {
        &self.secret
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerRole {
    Host,
    Client,
}

impl PeerRole {
    const fn file_name(self) -> &'static str {
        match self {
            Self::Host => HOST_FILE,
            Self::Client => CLIENT_FILE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileStamp {
    Missing,
    Present {
        modified: SystemTime,
        len: u64,
        inode: u64,
    },
}

impl FileStamp {
    fn of(path: &Path) -> io::Result<Self> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Self::Present {
                modified: metadata.modified()?,
                len: metadata.len(),
                inode: metadata.ino(),
            }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(Self::Missing),
            Err(source) => Err(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreExposure {
    ReadableByOthers { mode: u32 },
    OwnedByAnotherUser { uid: u32 },
}

impl fmt::Display for StoreExposure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadableByOthers { mode } => {
                write!(formatter, "is readable by other users (mode {mode:04o})")
            }
            Self::OwnedByAnotherUser { uid } => {
                write!(formatter, "is owned by another user (uid {uid})")
            }
        }
    }
}

impl StoreExposure {
    fn of(metadata: &fs::Metadata) -> Option<Self> {
        let uid = metadata.uid();
        if uid != rustix::process::geteuid().as_raw() {
            return Some(Self::OwnedByAnotherUser { uid });
        }

        let mode = metadata.mode() & PERMISSION_BITS;
        (mode & GROUP_AND_OTHER_BITS != 0).then_some(Self::ReadableByOthers { mode })
    }
}

struct Loaded {
    records: Vec<PeerRecord>,
    stamp: Option<FileStamp>,
    exposure: Option<StoreExposure>,
}

#[derive(Debug, Clone)]
pub struct PeerStore {
    path: PathBuf,
    records: Vec<PeerRecord>,
    stamp: Option<FileStamp>,
    exposure: Option<StoreExposure>,
}

impl PeerStore {
    pub fn open(path: PathBuf) -> Result<Self, NetError> {
        let loaded = load(&path)?;

        Ok(Self {
            path,
            records: loaded.records,
            stamp: loaded.stamp,
            exposure: loaded.exposure,
        })
    }

    pub const fn exposure(&self) -> Option<StoreExposure> {
        self.exposure
    }

    pub fn is_stale(&self) -> bool {
        FileStamp::of(&self.path).ok() != self.stamp || self.stamp.is_none()
    }

    pub fn refresh(&mut self) -> Result<bool, NetError> {
        if !self.is_stale() {
            return Ok(false);
        }

        let loaded = load(&self.path)?;
        self.records = loaded.records;
        self.stamp = loaded.stamp;
        self.exposure = loaded.exposure;
        Ok(true)
    }

    pub fn open_host(config: &PeersConfig) -> Result<Self, NetError> {
        Self::open_for(PeerRole::Host, config.host_file.as_ref())
    }

    pub fn open_client(config: &PeersConfig) -> Result<Self, NetError> {
        Self::open_for(PeerRole::Client, config.client_file.as_ref())
    }

    fn open_for(role: PeerRole, configured: Option<&PathBuf>) -> Result<Self, NetError> {
        match configured {
            Some(path) => Self::open(path.clone()),
            None => Self::open_in_state(role, env::var_os("XDG_STATE_HOME"), env::var_os("HOME")),
        }
    }

    fn open_in_state(
        role: PeerRole,
        xdg_state_home: Option<OsString>,
        home: Option<OsString>,
    ) -> Result<Self, NetError> {
        let path = default_path(role, xdg_state_home, home)?;

        if let Some(directory) = path.parent() {
            tighten_owned_directory(directory).map_err(|source| NetError::PeerStoreDirectory {
                path: directory.to_owned(),
                source,
            })?;
        }

        Self::open(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PeerRecord> {
        self.records.iter()
    }

    pub fn get(&self, id: &PeerId) -> Option<&PeerRecord> {
        self.records.iter().find(|record| record.id == *id)
    }

    pub fn remember(&mut self, record: PeerRecord) -> Result<(), NetError> {
        if self.records.len() >= MAX_PEERS && self.get(&record.id).is_none() {
            return Err(NetError::PeerStoreFull {
                capacity: MAX_PEERS,
            });
        }

        let mut records: Vec<PeerRecord> = self
            .records
            .iter()
            .filter(|existing| existing.id != record.id)
            .cloned()
            .collect();
        records.push(record);

        self.persist(&records)?;
        self.records = records;
        self.stamp = FileStamp::of(&self.path).ok();
        Ok(())
    }

    pub fn forget(&mut self, id: &PeerId) -> Result<bool, NetError> {
        if self.get(id).is_none() {
            return Ok(false);
        }

        let records: Vec<PeerRecord> = self
            .records
            .iter()
            .filter(|existing| existing.id != *id)
            .cloned()
            .collect();

        self.persist(&records)?;
        self.records = records;
        self.stamp = FileStamp::of(&self.path).ok();
        Ok(true)
    }

    fn persist(&self, records: &[PeerRecord]) -> Result<(), NetError> {
        let mut text = render(records);
        let written = write_owner_only(&self.path, &text);
        text.zeroize();

        written.map_err(|source| NetError::PeerStoreWrite {
            path: self.path.clone(),
            source,
        })
    }
}

fn load(path: &Path) -> Result<Loaded, NetError> {
    let stamp = FileStamp::of(path).ok();
    let exposure = fs::metadata(path)
        .ok()
        .and_then(|metadata| StoreExposure::of(&metadata));

    let records = match fs::read_to_string(path) {
        Ok(mut text) => {
            let parsed = parse(&text, path);
            text.zeroize();
            parsed?
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(source) => {
            return Err(NetError::PeerStoreRead {
                path: path.to_owned(),
                source,
            });
        }
    };

    Ok(Loaded {
        records,
        stamp,
        exposure,
    })
}

fn tighten_owned_directory(directory: &Path) -> io::Result<()> {
    let metadata = match fs::metadata(directory) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(source),
    };

    let owned = metadata.uid() == rustix::process::geteuid().as_raw();
    if metadata.is_dir() && owned && metadata.mode() & GROUP_AND_OTHER_BITS != 0 {
        fs::set_permissions(directory, fs::Permissions::from_mode(OWNER_ONLY_DIRECTORY))?;
    }

    Ok(())
}

pub use rusplay_core::write_owner_only;

fn default_path(
    role: PeerRole,
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, NetError> {
    let state_home = xdg_state_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local").join("state"))
        })
        .ok_or(NetError::NoStateDirectory)?;

    Ok(state_home.join(APP_DIR).join(role.file_name()))
}

fn render(records: &[PeerRecord]) -> String {
    let mut text = String::new();

    for record in records {
        text.push_str(&record.id.to_string());
        text.push(' ');
        push_hex(&mut text, record.secret.as_bytes());
        text.push(' ');
        text.push_str(&record.name);
        text.push('\n');
    }

    text
}

fn push_hex(text: &mut String, bytes: &[u8]) {
    for byte in bytes {
        text.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        text.push(char::from_digit(u32::from(byte & 0xF), 16).unwrap_or('0'));
    }
}

fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }

    let mut out = [0; N];
    let (pairs, _) = text.as_bytes().as_chunks::<2>();
    for (slot, pair) in out.iter_mut().zip(pairs) {
        let high = char::from(pair[0]).to_digit(16)?;
        let low = char::from(pair[1]).to_digit(16)?;
        *slot = u8::try_from(high << 4 | low).ok()?;
    }

    Some(out)
}

fn parse(text: &str, path: &Path) -> Result<Vec<PeerRecord>, NetError> {
    let mut records: Vec<PeerRecord> = Vec::new();

    for (index, line) in text.lines().enumerate() {
        if index >= MAX_PEERS {
            return Err(NetError::PeerStoreTooLarge {
                path: path.to_owned(),
                capacity: MAX_PEERS,
            });
        }

        let corrupt = || NetError::PeerStoreCorrupt {
            path: path.to_owned(),
            line: index + 1,
        };

        let mut fields = line.splitn(3, ' ');
        let (Some(id), Some(secret), Some(name)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(corrupt());
        };

        let id = decode_hex::<{ PeerId::LEN }>(id)
            .map(PeerId)
            .ok_or_else(corrupt)?;
        let mut secret_bytes = decode_hex::<PEER_SECRET_LEN>(secret).ok_or_else(corrupt)?;
        let secret = PeerSecret::from_bytes(secret_bytes);
        secret_bytes.zeroize();
        let record = PeerRecord::new(name, secret).map_err(|_| corrupt())?;

        if record.id != id || records.iter().any(|existing| existing.id == id) {
            return Err(corrupt());
        }

        records.push(record);
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(byte: u8) -> PeerSecret {
        PeerSecret::from_bytes([byte; PEER_SECRET_LEN])
    }

    fn record(name: &str, byte: u8) -> PeerRecord {
        PeerRecord::new(name, secret(byte)).unwrap()
    }

    fn store_text(count: usize) -> String {
        (0..count)
            .map(|index| {
                let mut bytes = [0; PEER_SECRET_LEN];
                bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
                render(&[PeerRecord::new("peer", PeerSecret::from_bytes(bytes)).unwrap()])
            })
            .collect()
    }

    #[test]
    fn a_full_store_refuses_a_new_peer_but_keeps_updating_known_ones() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        fs::write(&path, store_text(MAX_PEERS)).unwrap();
        let mut store = PeerStore::open(path).unwrap();

        let known = store.iter().next().unwrap().clone();
        let refused = store.remember(record("newcomer", 0xEE));
        let renamed = store.remember(PeerRecord::new("renamed", known.secret().clone()).unwrap());

        assert!(matches!(
            refused,
            Err(NetError::PeerStoreFull {
                capacity: MAX_PEERS
            })
        ));
        assert!(renamed.is_ok());
        assert_eq!(store.len(), MAX_PEERS);
    }

    #[test]
    fn a_store_file_beyond_the_capacity_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        fs::write(&path, store_text(MAX_PEERS + 1)).unwrap();

        assert!(matches!(
            PeerStore::open(path),
            Err(NetError::PeerStoreTooLarge {
                capacity: MAX_PEERS,
                ..
            })
        ));
    }

    #[test]
    fn ids_follow_the_secret() {
        assert_eq!(secret(1).id(), secret(1).id());
        assert_ne!(secret(1).id(), secret(2).id());
    }

    #[test]
    fn secret_debug_output_hides_the_bytes() {
        let shown = format!("{:?}", record("laptop", 0xAB));

        assert!(!shown.contains("171"));
        assert!(!shown.to_lowercase().contains("abab"));
    }

    #[test]
    fn names_are_validated() {
        assert!(matches!(
            PeerRecord::new("", secret(1)),
            Err(NetError::PeerNameEmpty)
        ));
        assert!(matches!(
            PeerRecord::new(&"x".repeat(MAX_PEER_NAME_BYTES + 1), secret(1)),
            Err(NetError::PeerNameTooLong { .. })
        ));
        assert!(matches!(
            PeerRecord::new("line\nbreak", secret(1)),
            Err(NetError::PeerNameControlCharacter)
        ));
        assert!(PeerRecord::new("living room pc", secret(1)).is_ok());
    }

    #[test]
    fn missing_file_opens_empty() {
        let directory = tempfile::tempdir().unwrap();

        let store = PeerStore::open(directory.path().join("peers")).unwrap();

        assert!(store.is_empty());
    }

    #[test]
    fn a_fresh_store_is_not_stale_and_an_external_change_is_picked_up() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        let mut running = PeerStore::open(path.clone()).unwrap();
        running.remember(record("laptop", 1)).unwrap();
        running.remember(record("desk", 2)).unwrap();

        let unchanged = running.refresh().unwrap();
        let mut other = PeerStore::open(path).unwrap();
        other.forget(&secret(1).id()).unwrap();
        let stale = running.is_stale();
        let changed = running.refresh().unwrap();

        assert!(!unchanged);
        assert!(stale);
        assert!(changed);
        assert!(running.get(&secret(1).id()).is_none());
        assert!(running.get(&secret(2).id()).is_some());
        assert!(!running.is_stale());
    }

    #[test]
    fn a_file_created_or_removed_behind_the_store_is_noticed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        let mut running = PeerStore::open(path.clone()).unwrap();
        let mut writer = PeerStore::open(path.clone()).unwrap();
        writer.remember(record("laptop", 1)).unwrap();

        let created = running.refresh().unwrap();
        fs::remove_file(&path).unwrap();
        let removed = running.refresh().unwrap();

        assert!(created);
        assert!(removed);
        assert!(running.is_empty());
    }

    #[test]
    fn a_corrupt_file_fails_the_refresh_and_keeps_the_old_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        let mut running = PeerStore::open(path.clone()).unwrap();
        running.remember(record("laptop", 1)).unwrap();

        fs::write(&path, "garbage line\n").unwrap();
        let result = running.refresh();

        assert!(matches!(result, Err(NetError::PeerStoreCorrupt { .. })));
        assert_eq!(running.len(), 1);
    }

    #[test]
    fn remembered_peers_survive_a_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("peers");
        let mut store = PeerStore::open(path.clone()).unwrap();

        store.remember(record("laptop", 1)).unwrap();
        store.remember(record("desk pc with spaces", 2)).unwrap();
        let reloaded = PeerStore::open(path).unwrap();

        let ids = |store: &PeerStore| -> Vec<(PeerId, String)> {
            store
                .iter()
                .map(|record| (record.id(), record.name().to_owned()))
                .collect()
        };

        assert_eq!(ids(&reloaded), ids(&store));
        assert_eq!(reloaded.len(), 2);
        assert_eq!(
            reloaded.get(&secret(2).id()).unwrap().name(),
            "desk pc with spaces"
        );
        assert_eq!(reloaded.get(&secret(1).id()).unwrap().secret(), &secret(1));
    }

    #[test]
    fn remembering_a_known_peer_replaces_its_entry() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PeerStore::open(directory.path().join("peers")).unwrap();

        store.remember(record("old name", 1)).unwrap();
        store.remember(record("new name", 1)).unwrap();

        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&secret(1).id()).unwrap().name(), "new name");
    }

    #[test]
    fn forgetting_removes_the_peer_on_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        let mut store = PeerStore::open(path.clone()).unwrap();
        store.remember(record("laptop", 1)).unwrap();
        store.remember(record("phone", 2)).unwrap();

        assert!(store.forget(&secret(1).id()).unwrap());
        assert!(!store.forget(&secret(1).id()).unwrap());
        let reloaded = PeerStore::open(path).unwrap();

        assert!(reloaded.get(&secret(1).id()).is_none());
        assert!(reloaded.get(&secret(2).id()).is_some());
    }

    #[test]
    fn files_are_owner_only_and_leave_no_staging_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("peers");
        let mut store = PeerStore::open(path.clone()).unwrap();

        store.remember(record("laptop", 1)).unwrap();
        store.remember(record("phone", 2)).unwrap();

        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let directory_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, OWNER_ONLY_FILE);
        assert_eq!(directory_mode, OWNER_ONLY_DIRECTORY);
        assert!(!path.with_extension(STAGING_SUFFIX).exists());
    }

    #[test]
    fn a_stale_permissive_staging_file_does_not_leak_into_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        let staging = path.with_extension(STAGING_SUFFIX);
        fs::write(&staging, "junk").unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o666)).unwrap();
        let mut store = PeerStore::open(path.clone()).unwrap();

        store.remember(record("laptop", 1)).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, OWNER_ONLY_FILE);
    }

    #[test]
    fn a_failed_write_leaves_the_store_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        fs::create_dir(path.with_extension(STAGING_SUFFIX)).unwrap();
        let mut store = PeerStore::open(path).unwrap();

        let result = store.remember(record("laptop", 1));

        assert!(matches!(result, Err(NetError::PeerStoreWrite { .. })));
        assert!(store.is_empty());
    }

    #[test]
    fn corrupt_files_are_refused_with_the_line_number() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        let good = render(&[record("laptop", 1)]);
        let id = secret(3).id();
        let mismatched = format!("{id} {} phone\n", "02".repeat(PEER_SECRET_LEN));
        let cases = [
            "garbage\n".to_owned(),
            format!("{good}short line\n"),
            format!("{good}{}", good.replace("01", "0g")),
            format!("{good}{good}"),
            mismatched,
        ];

        for text in cases {
            fs::write(&path, &text).unwrap();

            assert!(
                matches!(
                    PeerStore::open(path.clone()),
                    Err(NetError::PeerStoreCorrupt { .. })
                ),
                "{text:?}"
            );
        }

        fs::write(&path, format!("{good}oops\n")).unwrap();
        assert!(matches!(
            PeerStore::open(path),
            Err(NetError::PeerStoreCorrupt { line: 2, .. })
        ));
    }

    #[test]
    fn xdg_state_home_wins_and_home_is_the_fallback() {
        let xdg = default_path(
            PeerRole::Host,
            Some("/state".into()),
            Some("/home/user".into()),
        )
        .unwrap();
        let home = default_path(PeerRole::Client, None, Some("/home/user".into())).unwrap();
        let relative = default_path(
            PeerRole::Client,
            Some("state".into()),
            Some("/home/user".into()),
        )
        .unwrap();

        assert_eq!(xdg, PathBuf::from("/state/rusplay/paired-clients"));
        assert_eq!(
            home,
            PathBuf::from("/home/user/.local/state/rusplay/paired-hosts")
        );
        assert_eq!(relative, home);
        assert!(matches!(
            default_path(PeerRole::Host, None, None),
            Err(NetError::NoStateDirectory)
        ));
    }

    #[test]
    fn an_existing_permissive_state_directory_is_tightened_on_open() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("rusplay");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();

        PeerStore::open_in_state(
            PeerRole::Host,
            Some(directory.path().as_os_str().to_owned()),
            None,
        )
        .unwrap();

        let mode = fs::metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, OWNER_ONLY_DIRECTORY);
    }

    #[test]
    fn a_missing_state_directory_is_left_for_the_first_write() {
        let directory = tempfile::tempdir().unwrap();

        let store = PeerStore::open_in_state(
            PeerRole::Client,
            Some(directory.path().as_os_str().to_owned()),
            None,
        )
        .unwrap();

        assert!(store.is_empty());
        assert!(!directory.path().join("rusplay").exists());
    }

    #[test]
    fn a_group_or_world_readable_store_reports_its_exposure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers");
        let mut store = PeerStore::open(path.clone()).unwrap();
        store.remember(record("laptop", 1)).unwrap();
        let private = PeerStore::open(path.clone()).unwrap().exposure();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let shared = PeerStore::open(path).unwrap().exposure();

        assert_eq!(private, None);
        assert_eq!(
            shared,
            Some(StoreExposure::ReadableByOthers { mode: 0o644 })
        );
        assert_eq!(
            shared.unwrap().to_string(),
            "is readable by other users (mode 0644)"
        );
    }

    #[test]
    fn configured_paths_are_used() {
        let directory = tempfile::tempdir().unwrap();
        let config = PeersConfig {
            host_file: Some(directory.path().join("clients")),
            client_file: Some(directory.path().join("hosts")),
            ..PeersConfig::default()
        };

        assert_eq!(
            PeerStore::open_host(&config).unwrap().path(),
            directory.path().join("clients")
        );
        assert_eq!(
            PeerStore::open_client(&config).unwrap().path(),
            directory.path().join("hosts")
        );
    }
}
