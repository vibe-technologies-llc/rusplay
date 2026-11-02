use std::{
    env, fs, io,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use rusplay_config::PeersConfig;
use thiserror::Error;

const RUNTIME_DIRECTORY: &str = "rusplay";
const FILE_NAME: &str = "pin";
const TEMPORARY_SUFFIX: &str = ".tmp";
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

#[derive(Debug, Error)]
pub enum PinFileError {
    #[error(
        "no pin file path: peers.pin_file is unset and XDG_RUNTIME_DIR is not an absolute path"
    )]
    NoRuntimeDirectory,
    #[error("failed to create the pin directory {}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write the pin file {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("no pairing window is open, {} does not exist", path.display())]
    NoWindowOpen { path: PathBuf },
    #[error("failed to read the pin file {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn runtime_dir_from_process() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
}

pub fn resolve(peers: &PeersConfig, runtime_dir: Option<&Path>) -> Result<PathBuf, PinFileError> {
    if let Some(path) = &peers.pin_file {
        return Ok(path.clone());
    }

    runtime_dir
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(RUNTIME_DIRECTORY).join(FILE_NAME))
        .ok_or(PinFileError::NoRuntimeDirectory)
}

pub fn read(path: &Path) -> Result<String, PinFileError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text.trim().to_owned()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            Err(PinFileError::NoWindowOpen {
                path: path.to_owned(),
            })
        }
        Err(source) => Err(PinFileError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

#[derive(Debug)]
pub struct PinFile {
    path: PathBuf,
}

impl PinFile {
    pub fn open(
        peers: &PeersConfig,
        runtime_dir: Option<&Path>,
    ) -> Result<Option<Self>, PinFileError> {
        if !peers.write_pin_file {
            return Ok(None);
        }

        Ok(Some(Self {
            path: resolve(peers, runtime_dir)?,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn publish(&self, pin: &str) -> Result<(), PinFileError> {
        let write_error = |source| PinFileError::Write {
            path: self.path.clone(),
            source,
        };

        if let Some(parent) = self.path.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(DIRECTORY_MODE)
                .create(parent)
                .map_err(|source| PinFileError::CreateDirectory {
                    path: parent.to_owned(),
                    source,
                })?;
        }

        let mut temporary = self.path.clone().into_os_string();
        temporary.push(TEMPORARY_SUFFIX);
        let temporary = PathBuf::from(temporary);

        fs::remove_file(&temporary).ok();
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&temporary)
            .and_then(|mut file| writeln!(file, "{pin}"))
            .and_then(|()| fs::rename(&temporary, &self.path))
            .map_err(write_error)
    }

    pub fn clear(&self) {
        fs::remove_file(&self.path).ok();
    }
}

impl Drop for PinFile {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    fn enabled(path: Option<PathBuf>) -> PeersConfig {
        PeersConfig {
            write_pin_file: true,
            pin_file: path,
            ..PeersConfig::default()
        }
    }

    #[test]
    fn the_default_path_is_under_the_runtime_directory() {
        let path = resolve(&enabled(None), Some(Path::new("/run/user/1000"))).unwrap();

        assert_eq!(path, PathBuf::from("/run/user/1000/rusplay/pin"));
    }

    #[test]
    fn a_configured_path_wins_and_a_relative_runtime_directory_is_ignored() {
        let configured = enabled(Some(PathBuf::from("/srv/pin")));

        assert_eq!(
            resolve(&configured, None).unwrap(),
            PathBuf::from("/srv/pin")
        );
        assert!(matches!(
            resolve(&enabled(None), Some(Path::new("relative"))),
            Err(PinFileError::NoRuntimeDirectory)
        ));
        assert!(matches!(
            resolve(&enabled(None), None),
            Err(PinFileError::NoRuntimeDirectory)
        ));
    }

    #[test]
    fn nothing_is_opened_unless_the_option_is_on() {
        let off = PeersConfig::default();

        assert!(PinFile::open(&off, None).unwrap().is_none());
    }

    #[test]
    fn a_published_pin_is_private_and_readable_back() {
        let directory = TempDir::new().unwrap();
        let file = PinFile::open(&enabled(None), Some(directory.path()))
            .unwrap()
            .unwrap();

        file.publish("583014").unwrap();
        let file_mode = fs::metadata(file.path()).unwrap().permissions().mode() & 0o777;
        let directory_mode = fs::metadata(file.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(read(file.path()).unwrap(), "583014");
        assert_eq!(file_mode, FILE_MODE);
        assert_eq!(directory_mode, DIRECTORY_MODE);
    }

    #[test]
    fn a_new_pin_replaces_the_old_one_and_clearing_removes_it() {
        let directory = TempDir::new().unwrap();
        let file = PinFile::open(&enabled(None), Some(directory.path()))
            .unwrap()
            .unwrap();

        file.publish("111111").unwrap();
        file.publish("222222").unwrap();
        let replaced = read(file.path()).unwrap();
        file.clear();

        assert_eq!(replaced, "222222");
        assert!(matches!(
            read(file.path()),
            Err(PinFileError::NoWindowOpen { .. })
        ));
    }

    #[test]
    fn dropping_the_file_removes_the_pin() {
        let directory = TempDir::new().unwrap();
        let file = PinFile::open(&enabled(None), Some(directory.path()))
            .unwrap()
            .unwrap();
        let path = file.path().to_owned();

        file.publish("333333").unwrap();
        drop(file);

        assert!(!path.exists());
    }
}
