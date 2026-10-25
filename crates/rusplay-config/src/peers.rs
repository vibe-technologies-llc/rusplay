use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PeersConfig {
    pub host_file: Option<PathBuf>,
    pub client_file: Option<PathBuf>,
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

        Ok(())
    }
}
