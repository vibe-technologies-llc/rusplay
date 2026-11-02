use std::path::{Path, PathBuf};

use rusplay_config::{ConfigEnvironment, ConfigRole};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Explicit(PathBuf),
    Discovered(PathBuf),
    Defaults,
}

pub fn resolve(
    explicit: Option<PathBuf>,
    environment: &ConfigEnvironment,
    exists: impl Fn(&Path) -> bool,
) -> ConfigSource {
    if let Some(path) = explicit {
        return ConfigSource::Explicit(path);
    }

    environment
        .candidates(ConfigRole::Host)
        .into_iter()
        .find(|path| exists(path))
        .map_or(ConfigSource::Defaults, ConfigSource::Discovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XDG_FILE: &str = "/xdg/rusplay/host.toml";
    const HOME_FILE: &str = "/home/user/.config/rusplay/host.toml";

    fn environment(xdg: Option<&str>, home: Option<&str>) -> ConfigEnvironment {
        ConfigEnvironment {
            xdg_config_home: xdg.map(PathBuf::from),
            home: home.map(PathBuf::from),
        }
    }

    fn existing<'a>(files: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |path| files.iter().any(|file| Path::new(file) == path)
    }

    #[test]
    fn explicit_path_wins_without_being_checked() {
        let source = resolve(
            Some(PathBuf::from("/etc/missing.toml")),
            &environment(Some("/xdg"), Some("/home/user")),
            existing(&[XDG_FILE]),
        );

        assert_eq!(
            source,
            ConfigSource::Explicit(PathBuf::from("/etc/missing.toml"))
        );
    }

    #[test]
    fn xdg_config_home_wins_over_home() {
        let source = resolve(
            None,
            &environment(Some("/xdg"), Some("/home/user")),
            existing(&[XDG_FILE, HOME_FILE]),
        );

        assert_eq!(source, ConfigSource::Discovered(PathBuf::from(XDG_FILE)));
    }

    #[test]
    fn home_is_the_fallback_when_xdg_has_no_file() {
        let source = resolve(
            None,
            &environment(Some("/xdg"), Some("/home/user")),
            existing(&[HOME_FILE]),
        );

        assert_eq!(source, ConfigSource::Discovered(PathBuf::from(HOME_FILE)));
    }

    #[test]
    fn home_alone_is_searched_when_xdg_is_unset() {
        let source = resolve(
            None,
            &environment(None, Some("/home/user")),
            existing(&[HOME_FILE]),
        );

        assert_eq!(source, ConfigSource::Discovered(PathBuf::from(HOME_FILE)));
    }

    #[test]
    fn defaults_apply_when_no_file_exists() {
        let source = resolve(
            None,
            &environment(Some("/xdg"), Some("/home/user")),
            existing(&[]),
        );

        assert_eq!(source, ConfigSource::Defaults);
    }

    #[test]
    fn defaults_apply_without_any_environment() {
        let source = resolve(None, &ConfigEnvironment::default(), |_| true);

        assert_eq!(source, ConfigSource::Defaults);
    }
}
