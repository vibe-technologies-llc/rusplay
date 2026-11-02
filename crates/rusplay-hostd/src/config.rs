use rusplay_config::{Config, ConfigError};

use crate::discovery::ConfigSource;

pub fn default_toml() -> Result<String, ConfigError> {
    Config::default().to_toml_string()
}

pub fn check(source: &ConfigSource) -> Result<String, ConfigError> {
    match source {
        ConfigSource::Explicit(path) | ConfigSource::Discovered(path) => {
            Config::load(path)?;
            Ok(format!("{} is valid", path.display()))
        }
        ConfigSource::Defaults => {
            Ok("no config file found, the built-in defaults apply".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn file(text: &str) -> (TempDir, ConfigSource) {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("host.toml");
        fs::write(&path, text).unwrap();

        (directory, ConfigSource::Explicit(path))
    }

    #[test]
    fn the_default_document_loads_back_as_the_default_config() {
        let (_directory, source) = file(&default_toml().unwrap());

        let ConfigSource::Explicit(path) = &source else {
            panic!("expected a path");
        };

        assert_eq!(Config::load(path).unwrap(), Config::default());
        assert!(check(&source).unwrap().ends_with("is valid"));
    }

    #[test]
    fn a_misspelt_key_is_reported_as_a_parse_error() {
        let (_directory, source) = file("[network]\nprot = 1\n");

        assert!(matches!(check(&source), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn a_value_out_of_range_is_reported_by_validation() {
        let (_directory, source) = file("[network]\nmtu = 10\n");

        assert!(check(&source).is_err());
    }

    #[test]
    fn a_missing_file_is_a_read_error() {
        let source = ConfigSource::Explicit("/nonexistent/rusplay/host.toml".into());

        assert!(matches!(check(&source), Err(ConfigError::Read { .. })));
    }

    #[test]
    fn without_a_file_the_defaults_are_said_to_apply() {
        assert!(check(&ConfigSource::Defaults).unwrap().contains("defaults"));
    }
}
