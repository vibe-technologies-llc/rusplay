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
