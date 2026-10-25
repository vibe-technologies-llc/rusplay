use std::ops::Range;

use rusplay_core::{ColorPrimaries, GamutMapMode, HdrLevels, Luminance, TransferFunction};
use serde::{Deserialize, Serialize};

use crate::ConfigError;

const KNEE_RANGE: Range<f32> = 0.0..1.0;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ColorConfig {
    pub target: DisplayTargetConfig,
    pub gamut_mapping: GamutMappingConfig,
}

impl ColorConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.target.validate()?;
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
