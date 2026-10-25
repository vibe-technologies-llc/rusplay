use std::ops::RangeInclusive;

use rusplay_core::ScaleFilter;
use serde::{Deserialize, Serialize};

use crate::ConfigError;

const ANTIRINGING_RANGE: RangeInclusive<f32> = 0.0..=1.0;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScalingConfig {
    pub downscale: DownscaleConfig,
    pub upscale: UpscaleConfig,
}

impl ScalingConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if !ANTIRINGING_RANGE.contains(&self.upscale.antiringing) {
            return Err(ConfigError::AntiringingOutOfRange(self.upscale.antiringing));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DownscaleConfig {
    pub filter: ScaleFilter,
    pub chroma_filter: ScaleFilter,
    pub linear_light: bool,
}

impl Default for DownscaleConfig {
    fn default() -> Self {
        Self {
            filter: ScaleFilter::Hermite,
            chroma_filter: ScaleFilter::Hermite,
            linear_light: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpscaleConfig {
    pub filter: ScaleFilter,
    pub chroma_filter: ScaleFilter,
    pub sigmoid: bool,
    pub antiringing: f32,
}

impl Default for UpscaleConfig {
    fn default() -> Self {
        Self {
            filter: ScaleFilter::Lanczos3,
            chroma_filter: ScaleFilter::Lanczos3,
            sigmoid: true,
            antiringing: 0.0,
        }
    }
}
