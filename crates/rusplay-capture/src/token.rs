use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use rusplay_config::CaptureConfig;

use crate::CaptureError;

const APP_DIR: &str = "rusplay";
const TOKEN_FILE: &str = "restore-token";
const STAGING_SUFFIX: &str = "new";
const OWNER_ONLY: u32 = 0o600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreTokenStore {
    path: PathBuf,
}

impl RestoreTokenStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn from_config(config: &CaptureConfig) -> Result<Self, CaptureError> {
        match &config.restore_token_file {
            Some(path) => Ok(Self::new(path.clone())),
            None => default_path(env::var_os("XDG_STATE_HOME"), env::var_os("HOME")).map(Self::new),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<String>, CaptureError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                let token = contents.trim();
                Ok((!token.is_empty()).then(|| token.to_owned()))
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(CaptureError::RestoreTokenRead {
                path: self.path.clone(),
                source,
            }),
        }
    }

    pub fn store(&self, token: &str) -> Result<(), CaptureError> {
        let write_error = |source| CaptureError::RestoreTokenWrite {
            path: self.path.clone(),
            source,
        };

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(write_error)?;
        }

        let staging = self.path.with_extension(STAGING_SUFFIX);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(OWNER_ONLY)
            .open(&staging)
            .map_err(write_error)?;
        file.write_all(token.as_bytes()).map_err(write_error)?;
        file.sync_all().map_err(write_error)?;
        fs::rename(&staging, &self.path).map_err(write_error)
    }

    pub fn clear(&self) -> Result<(), CaptureError> {
        match fs::remove_file(&self.path) {
            Err(source) if source.kind() != io::ErrorKind::NotFound => {
                Err(CaptureError::RestoreTokenWrite {
                    path: self.path.clone(),
                    source,
                })
            }
            _ => Ok(()),
        }
    }
}

pub fn default_path(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, CaptureError> {
    let state_home = xdg_state_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local").join("state"))
        })
        .ok_or(CaptureError::NoStateDirectory)?;

    Ok(state_home.join(APP_DIR).join(TOKEN_FILE))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn xdg_state_home_wins() {
        let path = default_path(Some("/state".into()), Some("/home/user".into())).unwrap();

        assert_eq!(path, PathBuf::from("/state/rusplay/restore-token"));
    }

    #[test]
    fn home_is_the_fallback() {
        let unset = default_path(None, Some("/home/user".into())).unwrap();
        let relative = default_path(Some("state".into()), Some("/home/user".into())).unwrap();

        assert_eq!(
            unset,
            PathBuf::from("/home/user/.local/state/rusplay/restore-token")
        );
        assert_eq!(relative, unset);
    }

    #[test]
    fn no_directory_is_an_error() {
        assert!(matches!(
            default_path(None, None),
            Err(CaptureError::NoStateDirectory)
        ));
    }

    #[test]
    fn configured_path_is_used() {
        let config = CaptureConfig {
            restore_token_file: Some(PathBuf::from("/custom/token")),
            ..CaptureConfig::default()
        };

        let store = RestoreTokenStore::from_config(&config).unwrap();

        assert_eq!(store.path(), Path::new("/custom/token"));
    }

    #[test]
    fn token_round_trips_through_a_new_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = RestoreTokenStore::new(dir.path().join("rusplay").join("restore-token"));

        let before = store.load().unwrap();
        store.store("abc-123").unwrap();
        let after = store.load().unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();

        assert_eq!(before, None);
        assert_eq!(after.as_deref(), Some("abc-123"));
        assert_eq!(mode & 0o777, OWNER_ONLY);
    }

    #[test]
    fn storing_replaces_the_previous_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = RestoreTokenStore::new(dir.path().join("restore-token"));

        store.store("first").unwrap();
        store.store("second").unwrap();

        assert_eq!(store.load().unwrap().as_deref(), Some("second"));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn blank_file_holds_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = RestoreTokenStore::new(dir.path().join("restore-token"));

        fs::write(store.path(), " \n").unwrap();

        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn clearing_removes_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = RestoreTokenStore::new(dir.path().join("restore-token"));
        store.store("token").unwrap();

        store.clear().unwrap();
        store.clear().unwrap();

        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn unreadable_path_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = RestoreTokenStore::new(dir.path().to_owned());

        assert!(matches!(
            store.load(),
            Err(CaptureError::RestoreTokenRead { .. })
        ));
    }
}
