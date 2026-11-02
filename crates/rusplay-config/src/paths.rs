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

    pub fn candidates(&self, role: ConfigRole) -> Vec<PathBuf> {
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

        let mut candidates: Vec<PathBuf> = xdg
            .into_iter()
            .chain(home)
            .map(|base| base.join(CONFIG_DIRECTORY).join(role.file_name()))
            .collect();
        candidates.dedup();

        candidates
    }

    pub fn config_path(&self, role: ConfigRole) -> Result<PathBuf, ConfigError> {
        self.config_path_where(role, Path::is_file)
    }

    pub fn config_path_where(
        &self,
        role: ConfigRole,
        exists: impl Fn(&Path) -> bool,
    ) -> Result<PathBuf, ConfigError> {
        let candidates = self.candidates(role);

        candidates
            .iter()
            .find(|path| exists(path))
            .or_else(|| candidates.first())
            .cloned()
            .ok_or(ConfigError::NoConfigHome)
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

    const XDG_CLIENT: &str = "/xdg/rusplay/client.toml";
    const HOME_CLIENT: &str = "/home/user/.config/rusplay/client.toml";
    const HOME_HOST: &str = "/home/user/.config/rusplay/host.toml";

    fn existing<'a>(files: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |path| files.iter().any(|file| Path::new(file) == path)
    }

    #[test]
    fn candidates_list_xdg_before_home() {
        let candidates =
            environment(Some("/xdg"), Some("/home/user")).candidates(ConfigRole::Client);

        assert_eq!(
            candidates,
            [PathBuf::from(XDG_CLIENT), PathBuf::from(HOME_CLIENT)]
        );
    }

    #[test]
    fn xdg_config_home_is_preferred_when_nothing_exists() {
        let path = environment(Some("/xdg"), Some("/home/user"))
            .config_path_where(ConfigRole::Client, existing(&[]))
            .unwrap();

        assert_eq!(path, PathBuf::from(XDG_CLIENT));
    }

    #[test]
    fn xdg_file_wins_when_both_exist() {
        let path = environment(Some("/xdg"), Some("/home/user"))
            .config_path_where(ConfigRole::Client, existing(&[XDG_CLIENT, HOME_CLIENT]))
            .unwrap();

        assert_eq!(path, PathBuf::from(XDG_CLIENT));
    }

    #[test]
    fn home_file_is_used_when_only_it_exists() {
        let path = environment(Some("/xdg"), Some("/home/user"))
            .config_path_where(ConfigRole::Client, existing(&[HOME_CLIENT]))
            .unwrap();

        assert_eq!(path, PathBuf::from(HOME_CLIENT));
    }

    #[test]
    fn home_is_the_fallback() {
        let path = environment(None, Some("/home/user"))
            .config_path(ConfigRole::Host)
            .unwrap();

        assert_eq!(path, PathBuf::from(HOME_HOST));
    }

    #[test]
    fn relative_directories_are_ignored() {
        for xdg in ["relative", ""] {
            let candidates =
                environment(Some(xdg), Some("/home/user")).candidates(ConfigRole::Client);

            assert_eq!(candidates, [PathBuf::from(HOME_CLIENT)]);
        }
    }

    #[test]
    fn identical_candidates_collapse() {
        let candidates = environment(Some("/home/user/.config"), Some("/home/user"))
            .candidates(ConfigRole::Host);

        assert_eq!(candidates, [PathBuf::from(HOME_HOST)]);
    }

    #[test]
    fn no_directory_is_an_error() {
        let result = environment(None, None).config_path(ConfigRole::Client);

        assert!(matches!(result, Err(ConfigError::NoConfigHome)));
    }
}
