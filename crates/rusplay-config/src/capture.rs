use serde::{Deserialize, Serialize};

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
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            source: SourceKind::default(),
            cursor: CursorMode::default(),
            prefer_dmabuf: true,
            persist_session: true,
            restore_token: None,
        }
    }
}
