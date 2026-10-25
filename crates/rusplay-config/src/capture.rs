use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorMode {
    Hidden,
    Embedded,
    #[default]
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    #[default]
    Monitor,
    Window,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    pub source: SourceKind,
    pub cursor: CursorMode,
    pub prefer_dmabuf: bool,
    pub persist_session: bool,
    pub restore_token: Option<String>,
    pub restore_token_file: Option<PathBuf>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            source: SourceKind::default(),
            cursor: CursorMode::default(),
            prefer_dmabuf: true,
            persist_session: true,
            restore_token: None,
            restore_token_file: None,
        }
    }
}

impl CaptureConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        match &self.restore_token_file {
            Some(path) if path.as_os_str().is_empty() => Err(ConfigError::EmptyRestoreTokenFile),
            _ => Ok(()),
        }
    }
}
