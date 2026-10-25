use std::ops::{Range, RangeInclusive};

use rusplay_core::{
    ColorPrimaries, GamutMapMode, HdrLevels, Luminance, ToneMapOperator, TransferFunction,
};
use serde::{Deserialize, Serialize};

use crate::ConfigError;

const KNEE_RANGE: Range<f32> = 0.0..1.0;
const PERCENTILE_RANGE: RangeInclusive<f32> = f32::MIN_POSITIVE..=100.0;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ColorConfig {
    pub target: DisplayTargetConfig,
    pub tone_mapping: ToneMappingConfig,
    pub gamut_mapping: GamutMappingConfig,
}

impl ColorConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.target.validate()?;
        self.tone_mapping.validate()?;
        self.gamut_mapping.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayTargetConfig {
    pub primaries: Option<ColorPrimaries>,
    pub transfer: Option<TransferFunction>,
    pub reference_white_nits: Option<u32>,
    pub peak_nits: Option<u32>,
    pub black_nits: Option<Luminance>,
}

impl DisplayTargetConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        let in_range = |nits: u32| (1..=HdrLevels::MAX_NITS).contains(&nits);

        if let Some(white) = self.reference_white_nits.filter(|white| !in_range(*white)) {
            return Err(ConfigError::TargetReferenceWhiteOutOfRange(white));
        }
        if let Some(peak) = self.peak_nits.filter(|peak| !in_range(*peak)) {
            return Err(ConfigError::TargetPeakOutOfRange(peak));
        }
        if let (Some(black), Some(peak)) = (self.black_nits, self.peak_nits)
            && black.nits() >= f64::from(peak)
        {
            return Err(ConfigError::TargetBlackNotBelowPeak {
                black_nits: black.nits(),
                peak_nits: peak,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToneMappingConfig {
    pub operator: ToneMapOperator,
    pub dynamic_peak: bool,
    pub peak_percentile: f32,
    pub smoothing_frames: u32,
    pub scene_threshold_db: f32,
}

impl ToneMappingConfig {
    fn validate(self) -> Result<(), ConfigError> {
        if !PERCENTILE_RANGE.contains(&self.peak_percentile) {
            return Err(ConfigError::PeakPercentileOutOfRange(self.peak_percentile));
        }
        if self.smoothing_frames == 0 {
            return Err(ConfigError::ZeroSmoothingFrames);
        }
        if !(self.scene_threshold_db.is_finite() && self.scene_threshold_db > 0.0) {
            return Err(ConfigError::SceneThresholdOutOfRange(
                self.scene_threshold_db,
            ));
        }

        Ok(())
    }
}

impl Default for ToneMappingConfig {
    fn default() -> Self {
        Self {
            operator: ToneMapOperator::Bt2390,
            dynamic_peak: false,
            peak_percentile: 99.995,
            smoothing_frames: 20,
            scene_threshold_db: 3.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GamutMappingConfig {
    pub mode: GamutMapMode,
    pub knee: f32,
}

impl GamutMappingConfig {
    fn validate(self) -> Result<(), ConfigError> {
        if KNEE_RANGE.contains(&self.knee) {
            Ok(())
        } else {
            Err(ConfigError::GamutKneeOutOfRange(self.knee))
        }
    }
}

impl Default for GamutMappingConfig {
    fn default() -> Self {
        Self {
            mode: GamutMapMode::Perceptual,
            knee: 0.8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn target_overrides_default_to_unset() {
        let config = ColorConfig::default();

        assert_eq!(config.target, DisplayTargetConfig::default());
        assert_eq!(config.target.primaries, None);
        assert_eq!(config.gamut_mapping.mode, GamutMapMode::Perceptual);
    }

    #[test]
    fn target_overrides_parse_and_validate() {
        let parsed: Config = toml::from_str(
            r#"
            [color.target]
            primaries = "display-p3"
            transfer = "pq"
            reference_white_nits = 203
            peak_nits = 600
            black_nits = 0.05

            [color.gamut_mapping]
            mode = "desaturate"
            knee = 0.7
            "#,
        )
        .unwrap();
        let black_above_peak: Config = toml::from_str(
            r"
            [color.target]
            peak_nits = 100
            black_nits = 100.0
            ",
        )
        .unwrap();
        let dark_peak: Config = toml::from_str(
            r"
            [color.target]
            peak_nits = 0
            ",
        )
        .unwrap();

        assert_eq!(
            parsed.color.target.primaries,
            Some(ColorPrimaries::DisplayP3)
        );
        assert_eq!(parsed.color.gamut_mapping.mode, GamutMapMode::Desaturate);
        assert!(parsed.validate().is_ok());
        assert!(matches!(
            black_above_peak.validate(),
            Err(ConfigError::TargetBlackNotBelowPeak { .. })
        ));
        assert!(matches!(
            dark_peak.validate(),
            Err(ConfigError::TargetPeakOutOfRange(0))
        ));
    }

    #[test]
    fn tone_mapping_defaults_to_static_bt2390() {
        let config = ToneMappingConfig::default();

        assert_eq!(config.operator, ToneMapOperator::Bt2390);
        assert!(!config.dynamic_peak);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn tone_mapping_parses_and_validates() {
        let parsed: Config = toml::from_str(
            r#"
            [color.tone_mapping]
            operator = "bt2446a"
            dynamic_peak = true
            peak_percentile = 99.9
            smoothing_frames = 10
            scene_threshold_db = 2.0
            "#,
        )
        .unwrap();
        let invalid = [
            ToneMappingConfig {
                peak_percentile: 0.0,
                ..ToneMappingConfig::default()
            },
            ToneMappingConfig {
                smoothing_frames: 0,
                ..ToneMappingConfig::default()
            },
            ToneMappingConfig {
                scene_threshold_db: f32::NAN,
                ..ToneMappingConfig::default()
            },
        ];

        assert_eq!(parsed.color.tone_mapping.operator, ToneMapOperator::Bt2446a);
        assert!(parsed.color.tone_mapping.dynamic_peak);
        assert!(parsed.validate().is_ok());
        assert!(matches!(
            invalid[0].validate(),
            Err(ConfigError::PeakPercentileOutOfRange(_))
        ));
        assert!(matches!(
            invalid[1].validate(),
            Err(ConfigError::ZeroSmoothingFrames)
        ));
        assert!(matches!(
            invalid[2].validate(),
            Err(ConfigError::SceneThresholdOutOfRange(_))
        ));
    }

    #[test]
    fn knee_must_leave_room_to_compress() {
        let config = GamutMappingConfig {
            knee: 1.0,
            ..GamutMappingConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::GamutKneeOutOfRange(_))
        ));
    }
}
