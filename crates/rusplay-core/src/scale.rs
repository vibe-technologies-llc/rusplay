use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScaleFilter {
    Bilinear,
    Hermite,
    CatmullRom,
    Mitchell,
    Lanczos2,
    Lanczos3,
    Lanczos4,
    EwaLanczos,
    EwaLanczosSharp,
    EwaGinseng,
    EwaHann,
    EwaRobidoux,
    EwaRobidouxSharp,
}

impl ScaleFilter {
    pub const fn is_polar(self) -> bool {
        matches!(
            self,
            Self::EwaLanczos
                | Self::EwaLanczosSharp
                | Self::EwaGinseng
                | Self::EwaHann
                | Self::EwaRobidoux
                | Self::EwaRobidouxSharp
        )
    }
}

impl fmt::Display for ScaleFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bilinear => "bilinear",
            Self::Hermite => "hermite",
            Self::CatmullRom => "catmull-rom",
            Self::Mitchell => "mitchell",
            Self::Lanczos2 => "lanczos2",
            Self::Lanczos3 => "lanczos3",
            Self::Lanczos4 => "lanczos4",
            Self::EwaLanczos => "ewa-lanczos",
            Self::EwaLanczosSharp => "ewa-lanczos-sharp",
            Self::EwaGinseng => "ewa-ginseng",
            Self::EwaHann => "ewa-hann",
            Self::EwaRobidoux => "ewa-robidoux",
            Self::EwaRobidouxSharp => "ewa-robidoux-sharp",
        })
    }
}
