use std::{ops::RangeInclusive, path::PathBuf};

use rusplay_core::Resolution;
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
    Virtual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    pub source: SourceKind,
    pub cursor: CursorMode,
    pub prefer_dmabuf: bool,
    pub dmabuf_gpu_fence_wait: bool,
    pub dmabuf_fence_timeout_ms: u32,
    pub persist_session: bool,
    pub restore_token: Option<String>,
    pub restore_token_file: Option<PathBuf>,
    pub virtual_max_resolution: Resolution,
    pub virtual_max_refresh_hz: u32,
    pub virtual_min_resolution: Resolution,
    pub virtual_resize_min_interval_ms: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            source: SourceKind::default(),
            cursor: CursorMode::default(),
            prefer_dmabuf: true,
            dmabuf_gpu_fence_wait: true,
            dmabuf_fence_timeout_ms: 100,
            persist_session: true,
            restore_token: None,
            restore_token_file: None,
            virtual_max_resolution: Resolution::new(7680, 4320),
            virtual_max_refresh_hz: 240,
            virtual_min_resolution: Resolution::new(200, 200),
            virtual_resize_min_interval_ms: 250,
        }
    }
}

impl CaptureConfig {
    pub const MAX_VIRTUAL_REFRESH_HZ: u32 = 1000;
    pub const VIRTUAL_RESIZE_INTERVAL_RANGE_MS: RangeInclusive<u32> = 10..=60_000;

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.virtual_max_resolution.pixel_count() == 0 {
            return Err(ConfigError::EmptyVirtualMaxResolution {
                width: self.virtual_max_resolution.width,
                height: self.virtual_max_resolution.height,
            });
        }

        if !(1..=Self::MAX_VIRTUAL_REFRESH_HZ).contains(&self.virtual_max_refresh_hz) {
            return Err(ConfigError::VirtualMaxRefreshOutOfRange(
                self.virtual_max_refresh_hz,
            ));
        }

        if self.virtual_min_resolution.pixel_count() == 0
            || self.virtual_min_resolution.width > self.virtual_max_resolution.width
            || self.virtual_min_resolution.height > self.virtual_max_resolution.height
        {
            return Err(ConfigError::VirtualMinResolutionOutOfRange {
                width: self.virtual_min_resolution.width,
                height: self.virtual_min_resolution.height,
            });
        }

        if !Self::VIRTUAL_RESIZE_INTERVAL_RANGE_MS.contains(&self.virtual_resize_min_interval_ms) {
            return Err(ConfigError::VirtualResizeIntervalOutOfRange(
                self.virtual_resize_min_interval_ms,
            ));
        }

        match &self.restore_token_file {
            Some(path) if path.as_os_str().is_empty() => Err(ConfigError::EmptyRestoreTokenFile),
            _ => Ok(()),
        }
    }
}
