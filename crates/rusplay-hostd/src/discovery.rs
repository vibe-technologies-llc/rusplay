use std::{
    env,
    path::{Path, PathBuf},
};

const CONFIG_DIRECTORY: &str = "rusplay";
const CONFIG_FILE: &str = "host.toml";
const HOME_CONFIG_DIRECTORY: &str = ".config";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    pub xdg_config_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Environment {
    pub fn from_process() -> Self {
        Self {
            xdg_config_home: env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            home: env::var_os("HOME").map(PathBuf::from),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Explicit(PathBuf),
    Discovered(PathBuf),
    Defaults,
}

pub fn candidates(environment: &Environment) -> Vec<PathBuf> {
    let xdg = environment
        .xdg_config_home
        .as_deref()
        .filter(|path| path.is_absolute());
    let home = environment
        .home
        .as_deref()
        .filter(|path| path.is_absolute())
        .map(|home| home.join(HOME_CONFIG_DIRECTORY));

    let mut candidates: Vec<PathBuf> = xdg
        .into_iter()
        .map(Path::to_path_buf)
        .chain(home)
        .map(|base| base.join(CONFIG_DIRECTORY).join(CONFIG_FILE))
        .collect();
    candidates.dedup();

    candidates
}

pub fn resolve(
    explicit: Option<PathBuf>,
    environment: &Environment,
    exists: impl Fn(&Path) -> bool,
) -> ConfigSource {
    if let Some(path) = explicit {
        return ConfigSource::Explicit(path);
    }

    candidates(environment)
        .into_iter()
        .find(|path| exists(path))
        .map_or(ConfigSource::Defaults, ConfigSource::Discovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XDG_FILE: &str = "/xdg/rusplay/host.toml";
    const HOME_FILE: &str = "/home/user/.config/rusplay/host.toml";

    fn environment(xdg: Option<&str>, home: Option<&str>) -> Environment {
        Environment {
            xdg_config_home: xdg.map(PathBuf::from),
            home: home.map(PathBuf::from),
        }
    }

    fn existing<'a>(files: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |path| files.iter().any(|file| Path::new(file) == path)
    }

    #[test]
    fn candidates_list_xdg_before_home() {
        let candidates = candidates(&environment(Some("/xdg"), Some("/home/user")));

        assert_eq!(
            candidates,
            [PathBuf::from(XDG_FILE), PathBuf::from(HOME_FILE)]
        );
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
        let source = resolve(None, &Environment::default(), |_| true);

        assert_eq!(source, ConfigSource::Defaults);
    }

    #[test]
    fn relative_and_empty_directories_are_ignored() {
        for xdg in ["relative/dir", ""] {
            let candidates = candidates(&environment(Some(xdg), Some("/home/user")));

            assert_eq!(candidates, [PathBuf::from(HOME_FILE)]);
        }
    }

    #[test]
    fn identical_candidates_collapse() {
        let candidates = candidates(&environment(Some("/home/user/.config"), Some("/home/user")));

        assert_eq!(candidates, [PathBuf::from(HOME_FILE)]);
    }
}
