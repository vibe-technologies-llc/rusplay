use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GamutMapMode {
    #[default]
    Perceptual,
    Relative,
    Absolute,
    Saturation,
    Desaturate,
    Highlight,
}

impl GamutMapMode {
    pub const ALL: [Self; 6] = [
        Self::Perceptual,
        Self::Relative,
        Self::Absolute,
        Self::Saturation,
        Self::Desaturate,
        Self::Highlight,
    ];
}

impl fmt::Display for GamutMapMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Perceptual => "perceptual",
            Self::Relative => "relative",
            Self::Absolute => "absolute",
            Self::Saturation => "saturation",
            Self::Desaturate => "desaturate",
            Self::Highlight => "highlight",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToneMapOperator {
    #[default]
    Bt2390,
    Bt2446a,
    Spline,
    Reinhard,
    Clip,
}

impl ToneMapOperator {
    pub const ALL: [Self; 5] = [
        Self::Bt2390,
        Self::Bt2446a,
        Self::Spline,
        Self::Reinhard,
        Self::Clip,
    ];
}

impl fmt::Display for ToneMapOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bt2390 => "bt2390",
            Self::Bt2446a => "bt2446a",
            Self::Spline => "spline",
            Self::Reinhard => "reinhard",
            Self::Clip => "clip",
        })
    }
}
