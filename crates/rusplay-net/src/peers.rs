use std::{
    env,
    ffi::OsString,
    fmt, fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use rusplay_config::PeersConfig;
use rusplay_protocol::PeerId;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

use crate::NetError;

pub const PEER_SECRET_LEN: usize = 32;
pub const MAX_PEER_NAME_BYTES: usize = 64;

const APP_DIR: &str = "rusplay";
const HOST_FILE: &str = "paired-clients";
const CLIENT_FILE: &str = "paired-hosts";
const STAGING_SUFFIX: &str = "new";
const OWNER_ONLY_FILE: u32 = 0o600;
const OWNER_ONLY_DIRECTORY: u32 = 0o700;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerStore {
    path: PathBuf,
    records: Vec<PeerRecord>,
}

impl PeerStore {
    pub fn open(path: PathBuf) -> Result<Self, NetError> {
        let records = match fs::read_to_string(&path) {
            Ok(text) => parse(&text, &path)?,
            Err(source) if source.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(NetError::PeerStoreRead { path, source }),
        };

        Ok(Self { path, records })
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
            None => default_path(role, env::var_os("XDG_STATE_HOME"), env::var_os("HOME"))
                .and_then(Self::open),
        }
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
        let mut records: Vec<PeerRecord> = self
            .records
            .iter()
            .filter(|existing| existing.id != record.id)
            .cloned()
            .collect();
        records.push(record);

        self.persist(&records)?;
        self.records = records;
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

pub fn write_owner_only(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(OWNER_ONLY_DIRECTORY)
            .create(parent)?;
    }

    let staging = path.with_added_extension(STAGING_SUFFIX);
    match fs::remove_file(&staging) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => return Err(source),
        _ => {}
    }

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(OWNER_ONLY_FILE)
        .open(&staging)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;

    fs::rename(&staging, path)
}

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
        let secret = decode_hex::<PEER_SECRET_LEN>(secret)
            .map(PeerSecret::from_bytes)
            .ok_or_else(corrupt)?;
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
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn secret(byte: u8) -> PeerSecret {
        PeerSecret::from_bytes([byte; PEER_SECRET_LEN])
    }

    fn record(name: &str, byte: u8) -> PeerRecord {
        PeerRecord::new(name, secret(byte)).unwrap()
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
    fn remembered_peers_survive_a_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state").join("peers");
        let mut store = PeerStore::open(path.clone()).unwrap();

        store.remember(record("laptop", 1)).unwrap();
        store.remember(record("desk pc with spaces", 2)).unwrap();
        let reloaded = PeerStore::open(path).unwrap();

        assert_eq!(reloaded, store);
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
