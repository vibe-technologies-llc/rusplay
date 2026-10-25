use serde::{Deserialize, Serialize};

use crate::color::{ColorError, ColorPrimaries};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Luminance(u32);

impl Luminance {
    pub const UNITS_PER_NIT: u32 = 10_000;
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(HdrLevels::MAX_NITS * Self::UNITS_PER_NIT);

    pub const fn from_units(units: u32) -> Option<Self> {
        if units > Self::MAX.0 {
            None
        } else {
            Some(Self(units))
        }
    }

    pub const fn from_whole_nits(nits: u32) -> Option<Self> {
        match nits.checked_mul(Self::UNITS_PER_NIT) {
            Some(units) => Self::from_units(units),
            None => None,
        }
    }

    pub fn from_nits(nits: f64) -> Result<Self, ColorError> {
        let units = (nits * f64::from(Self::UNITS_PER_NIT)).round();
        if !(0.0..=f64::from(Self::MAX.0)).contains(&units) {
            return Err(ColorError::LuminanceOutOfRange(nits));
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = units as u32;
        Ok(Self(whole))
    }

    pub const fn units(self) -> u32 {
        self.0
    }

    pub fn nits(self) -> f64 {
        f64::from(self.0) / f64::from(Self::UNITS_PER_NIT)
    }
}

impl TryFrom<f64> for Luminance {
    type Error = ColorError;

    fn try_from(nits: f64) -> Result<Self, ColorError> {
        Self::from_nits(nits)
    }
}

impl From<Luminance> for f64 {
    fn from(luminance: Luminance) -> Self {
        luminance.nits()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MasteringDisplay {
    pub primaries: ColorPrimaries,
    pub max_nits: Luminance,
    pub min_nits: Luminance,
}

impl MasteringDisplay {
    pub fn is_ordered(&self) -> bool {
        self.min_nits < self.max_nits
    }
}

impl Default for MasteringDisplay {
    fn default() -> Self {
        Self {
            primaries: ColorPrimaries::DisplayP3,
            max_nits: Luminance(1000 * Luminance::UNITS_PER_NIT),
            min_nits: Luminance(50),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContentLightLevel {
    pub max_cll_nits: u16,
    pub max_fall_nits: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HdrLevels {
    pub reference_white_nits: u32,
    pub peak_nits: u32,
    pub mastering: Option<MasteringDisplay>,
    pub content_light: Option<ContentLightLevel>,
}

impl HdrLevels {
    pub const MAX_NITS: u32 = 10_000;
    pub const BT2408_REFERENCE_WHITE_NITS: u32 = 203;

    pub const fn reference_white_in_range(self) -> bool {
        self.reference_white_nits >= 1 && self.reference_white_nits <= Self::MAX_NITS
    }

    pub const fn peak_in_range(self) -> bool {
        self.peak_nits >= 1 && self.peak_nits <= Self::MAX_NITS
    }

    pub fn content_peak_nits(&self) -> f64 {
        let max_cll = self
            .content_light
            .map(|light| f64::from(light.max_cll_nits))
            .filter(|nits| *nits > 0.0);
        let mastering = self
            .mastering
            .map(|display| display.max_nits.nits())
            .filter(|nits| *nits > 0.0);

        max_cll
            .or(mastering)
            .unwrap_or_else(|| f64::from(self.peak_nits))
            .min(f64::from(Self::MAX_NITS))
    }
}

impl Default for HdrLevels {
    fn default() -> Self {
        Self {
            reference_white_nits: Self::BT2408_REFERENCE_WHITE_NITS,
            peak_nits: 1000,
            mastering: None,
            content_light: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_peak_prefers_max_cll_then_mastering_then_peak() {
        let mut levels = HdrLevels {
            peak_nits: 600,
            ..HdrLevels::default()
        };

        assert!((levels.content_peak_nits() - 600.0).abs() < f64::EPSILON);

        levels.mastering = Some(MasteringDisplay {
            max_nits: Luminance::from_whole_nits(4000).unwrap(),
            ..MasteringDisplay::default()
        });
        assert!((levels.content_peak_nits() - 4000.0).abs() < f64::EPSILON);

        levels.content_light = Some(ContentLightLevel {
            max_cll_nits: 1500,
            max_fall_nits: 400,
        });
        assert!((levels.content_peak_nits() - 1500.0).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_max_cll_falls_through() {
        let levels = HdrLevels {
            content_light: Some(ContentLightLevel::default()),
            ..HdrLevels::default()
        };

        assert!((levels.content_peak_nits() - 1000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn luminance_uses_st2086_units() {
        let luminance = Luminance::from_nits(0.005).unwrap();

        assert_eq!(luminance.units(), 50);
        assert!(Luminance::from_nits(-1.0).is_err());
        assert!(Luminance::from_nits(10_001.0).is_err());
    }
}
