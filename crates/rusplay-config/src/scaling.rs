use std::ops::RangeInclusive;

use rusplay_core::ScaleFilter;
use serde::{Deserialize, Serialize};

use crate::ConfigError;

const ANTIRINGING_RANGE: RangeInclusive<f32> = 0.0..=1.0;
const SIGMOID_CENTER_RANGE: RangeInclusive<f32> = 0.0..=1.0;
const SIGMOID_SLOPE_RANGE: RangeInclusive<f32> = 1.0..=20.0;

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
        if !SIGMOID_CENTER_RANGE.contains(&self.upscale.sigmoid_center) {
            return Err(ConfigError::SigmoidCenterOutOfRange(
                self.upscale.sigmoid_center,
            ));
        }
        if !SIGMOID_SLOPE_RANGE.contains(&self.upscale.sigmoid_slope) {
            return Err(ConfigError::SigmoidSlopeOutOfRange(
                self.upscale.sigmoid_slope,
            ));
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
    pub sigmoid_center: f32,
    pub sigmoid_slope: f32,
    pub antiringing: f32,
}

impl Default for UpscaleConfig {
    fn default() -> Self {
        Self {
            filter: ScaleFilter::Lanczos3,
            chroma_filter: ScaleFilter::Lanczos3,
            sigmoid: true,
            sigmoid_center: 0.75,
            sigmoid_slope: 6.5,
            antiringing: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upscale(sigmoid_center: f32, sigmoid_slope: f32) -> ScalingConfig {
        ScalingConfig {
            upscale: UpscaleConfig {
                sigmoid_center,
                sigmoid_slope,
                ..UpscaleConfig::default()
            },
            ..ScalingConfig::default()
        }
    }

    #[test]
    fn sigmoid_defaults_follow_libplacebo() {
        let defaults = UpscaleConfig::default();

        assert!(defaults.sigmoid);
        assert!((defaults.sigmoid_center - 0.75).abs() < f32::EPSILON);
        assert!((defaults.sigmoid_slope - 6.5).abs() < f32::EPSILON);
    }

    #[test]
    fn sigmoid_parameters_are_validated() {
        assert!(upscale(0.0, 1.0).validate().is_ok());
        assert!(upscale(1.0, 20.0).validate().is_ok());
        assert!(matches!(
            upscale(1.25, 6.5).validate(),
            Err(ConfigError::SigmoidCenterOutOfRange(center)) if (center - 1.25).abs() < f32::EPSILON
        ));
        assert!(matches!(
            upscale(0.75, 0.5).validate(),
            Err(ConfigError::SigmoidSlopeOutOfRange(slope)) if (slope - 0.5).abs() < f32::EPSILON
        ));
    }
}
