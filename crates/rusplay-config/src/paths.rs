use std::{
    env,
    path::{Path, PathBuf},
};

use crate::ConfigError;

const CONFIG_DIRECTORY: &str = "rusplay";
const HOME_CONFIG_DIRECTORY: &str = ".config";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRole {
    Client,
    Host,
}

impl ConfigRole {
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Client => "client.toml",
            Self::Host => "host.toml",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigEnvironment {
    pub xdg_config_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl ConfigEnvironment {
    pub fn from_process() -> Self {
        Self {
            xdg_config_home: env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            home: env::var_os("HOME").map(PathBuf::from),
        }
    }

    pub fn config_path(&self, role: ConfigRole) -> Result<PathBuf, ConfigError> {
        let xdg = self
            .xdg_config_home
            .as_deref()
            .filter(|path| path.is_absolute())
            .map(Path::to_path_buf);
        let home = self
            .home
            .as_deref()
            .filter(|path| path.is_absolute())
            .map(|home| home.join(HOME_CONFIG_DIRECTORY));

        let base = xdg.or(home).ok_or(ConfigError::NoConfigHome)?;

        Ok(base.join(CONFIG_DIRECTORY).join(role.file_name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(xdg: Option<&str>, home: Option<&str>) -> ConfigEnvironment {
        ConfigEnvironment {
            xdg_config_home: xdg.map(PathBuf::from),
            home: home.map(PathBuf::from),
        }
    }

    #[test]
    fn xdg_config_home_wins() {
        let path = environment(Some("/xdg"), Some("/home/user"))
            .config_path(ConfigRole::Client)
            .unwrap();

        assert_eq!(path, PathBuf::from("/xdg/rusplay/client.toml"));
    }

    #[test]
    fn home_is_the_fallback() {
        let path = environment(None, Some("/home/user"))
            .config_path(ConfigRole::Host)
            .unwrap();

        assert_eq!(path, PathBuf::from("/home/user/.config/rusplay/host.toml"));
    }

    #[test]
    fn relative_directories_are_ignored() {
        let path = environment(Some("relative"), Some("/home/user"))
            .config_path(ConfigRole::Client)
            .unwrap();

        assert_eq!(
            path,
            PathBuf::from("/home/user/.config/rusplay/client.toml")
        );
    }

    #[test]
    fn no_directory_is_an_error() {
        let result = environment(None, None).config_path(ConfigRole::Client);

        assert!(matches!(result, Err(ConfigError::NoConfigHome)));
    }
}
