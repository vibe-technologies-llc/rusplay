use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

const MAX_HOST_NAME_BYTES: usize = 64;
const MAX_CLIENT_NAME_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PeersConfig {
    pub host_file: Option<PathBuf>,
    pub client_file: Option<PathBuf>,
    pub host_name: Option<String>,
    pub client_name: Option<String>,
    pub write_pin_file: bool,
    pub pin_file: Option<PathBuf>,
    pub control_socket: bool,
    pub control_socket_path: Option<PathBuf>,
    pub host_on_launch: bool,
}

impl Default for PeersConfig {
    fn default() -> Self {
        Self {
            host_file: None,
            client_file: None,
            host_name: None,
            client_name: None,
            write_pin_file: false,
            pin_file: None,
            control_socket: true,
            control_socket_path: None,
            host_on_launch: false,
        }
    }
}

impl PeersConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self
            .host_file
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ConfigError::EmptyHostPeersFile);
        }

        if self
            .client_file
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ConfigError::EmptyClientPeersFile);
        }

        if self
            .pin_file
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ConfigError::EmptyPinFile);
        }

        if self
            .control_socket_path
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ConfigError::EmptyControlSocketPath);
        }

        match self.host_name.as_deref() {
            Some("") => return Err(ConfigError::EmptyHostName),
            Some(name) if name.len() > MAX_HOST_NAME_BYTES => {
                return Err(ConfigError::HostNameTooLong { bytes: name.len() });
            }
            _ => {}
        }

        match self.client_name.as_deref() {
            Some("") => Err(ConfigError::EmptyClientName),
            Some(name) if name.len() > MAX_CLIENT_NAME_BYTES => {
                Err(ConfigError::ClientNameTooLong { bytes: name.len() })
            }
            Some(name) if name.chars().any(char::is_control) => {
                Err(ConfigError::ClientNameControlCharacter)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> PeersConfig {
        PeersConfig {
            host_name: Some(name.to_owned()),
            ..PeersConfig::default()
        }
    }

    #[test]
    fn host_name_is_unset_by_default() {
        assert_eq!(PeersConfig::default().host_name, None);
        assert!(PeersConfig::default().validate().is_ok());
    }

    #[test]
    fn empty_and_oversized_host_names_are_rejected() {
        assert!(matches!(
            named("").validate(),
            Err(ConfigError::EmptyHostName)
        ));
        assert!(matches!(
            named(&"n".repeat(65)).validate(),
            Err(ConfigError::HostNameTooLong { bytes: 65 })
        ));
        assert!(named(&"n".repeat(64)).validate().is_ok());
    }

    #[test]
    fn the_pin_file_is_off_by_default_and_an_empty_path_is_rejected() {
        let empty = PeersConfig {
            pin_file: Some(PathBuf::new()),
            ..PeersConfig::default()
        };

        assert!(!PeersConfig::default().write_pin_file);
        assert_eq!(PeersConfig::default().pin_file, None);
        assert!(matches!(empty.validate(), Err(ConfigError::EmptyPinFile)));
    }

    #[test]
    fn the_control_socket_is_on_by_default_and_an_empty_path_is_rejected() {
        let empty = PeersConfig {
            control_socket_path: Some(PathBuf::new()),
            ..PeersConfig::default()
        };

        assert!(PeersConfig::default().control_socket);
        assert_eq!(PeersConfig::default().control_socket_path, None);
        assert!(!PeersConfig::default().host_on_launch);
        assert!(matches!(
            empty.validate(),
            Err(ConfigError::EmptyControlSocketPath)
        ));
    }

    fn client_named(name: &str) -> PeersConfig {
        PeersConfig {
            client_name: Some(name.to_owned()),
            ..PeersConfig::default()
        }
    }

    #[test]
    fn client_name_is_unset_by_default() {
        assert_eq!(PeersConfig::default().client_name, None);
    }

    #[test]
    fn empty_oversized_and_control_client_names_are_rejected() {
        assert!(matches!(
            client_named("").validate(),
            Err(ConfigError::EmptyClientName)
        ));
        assert!(matches!(
            client_named(&"n".repeat(65)).validate(),
            Err(ConfigError::ClientNameTooLong { bytes: 65 })
        ));
        assert!(matches!(
            client_named("two\nlines").validate(),
            Err(ConfigError::ClientNameControlCharacter)
        ));
        assert!(client_named(&"n".repeat(64)).validate().is_ok());
    }
}
