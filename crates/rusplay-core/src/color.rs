mod hdr;
mod mapping;
mod matrix;
mod primaries;
mod signalled;
mod transfer;

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use crate::color::{
    hdr::{ContentLightLevel, HdrLevels, Luminance, MasteringDisplay},
    mapping::{GamutMapMode, ToneMapOperator},
    matrix::{Matrix3, bradford_adaptation},
    primaries::{Chromaticities, Chromaticity, ColorPrimaries, WhiteAdaptation},
    signalled::SignalledColor,
    transfer::{TransferFunction, pq_from_nits, pq_to_nits},
};

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ColorError {
    #[error("chromaticity ({x}, {y}) is outside 0..=1 or has y = 0")]
    ChromaticityOutOfRange { x: f64, y: f64 },
    #[error("the primaries do not span a gamut containing their white point")]
    DegenerateGamut,
    #[error("luminance {0} cd/m² is outside 0..=10000")]
    LuminanceOutOfRange(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatrixCoefficients {
    #[default]
    Bt709,
    Bt601,
    Bt2020Ncl,
}

impl MatrixCoefficients {
    pub const ALL: [Self; 3] = [Self::Bt709, Self::Bt601, Self::Bt2020Ncl];

    pub const fn for_primaries(primaries: ColorPrimaries) -> Self {
        match primaries {
            ColorPrimaries::Bt2020 => Self::Bt2020Ncl,
            ColorPrimaries::Bt601_525 | ColorPrimaries::Bt601_625 => Self::Bt601,
            ColorPrimaries::Bt709
            | ColorPrimaries::DisplayP3
            | ColorPrimaries::DciP3
            | ColorPrimaries::Custom(_) => Self::Bt709,
        }
    }

    pub const fn h273_code(self) -> u8 {
        match self {
            Self::Bt709 => 1,
            Self::Bt601 => 6,
            Self::Bt2020Ncl => 9,
        }
    }

    pub fn from_h273_code(code: u8) -> Option<Self> {
        match code {
            5 => Some(Self::Bt601),
            _ => Self::ALL
                .into_iter()
                .find(|matrix| matrix.h273_code() == code),
        }
    }

    pub const fn red_blue_coefficients(self) -> (f64, f64) {
        match self {
            Self::Bt709 => (0.2126, 0.0722),
            Self::Bt601 => (0.299, 0.114),
            Self::Bt2020Ncl => (0.2627, 0.0593),
        }
    }

    pub fn rows(self) -> [[f64; 3]; 3] {
        let (red, blue) = self.red_blue_coefficients();
        let green = 1.0 - red - blue;
        let blue_scale = 2.0 * (1.0 - blue);
        let red_scale = 2.0 * (1.0 - red);

        [
            [red, green, blue],
            [-red / blue_scale, -green / blue_scale, 0.5],
            [0.5, -green / red_scale, -blue / red_scale],
        ]
    }

    pub fn inverse_rows(self) -> [[f64; 3]; 3] {
        let (red, blue) = self.red_blue_coefficients();
        let green = 1.0 - red - blue;
        let blue_scale = 2.0 * (1.0 - blue);
        let red_scale = 2.0 * (1.0 - red);

        [
            [1.0, 0.0, red_scale],
            [1.0, -blue * blue_scale / green, -red * red_scale / green],
            [1.0, blue_scale, 0.0],
        ]
    }
}

impl fmt::Display for MatrixCoefficients {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bt709 => "bt709",
            Self::Bt601 => "bt601",
            Self::Bt2020Ncl => "bt2020-ncl",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorRange {
    Limited,
    #[default]
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ColorSpace {
    pub primaries: ColorPrimaries,
    pub transfer: TransferFunction,
    pub matrix: Option<MatrixCoefficients>,
    pub range: ColorRange,
    pub hdr: HdrLevels,
}

impl ColorSpace {
    pub fn matrix_coefficients(&self) -> MatrixCoefficients {
        self.matrix
            .unwrap_or_else(|| MatrixCoefficients::for_primaries(self.primaries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_defaults_follow_the_primaries() {
        let bt2020 = ColorSpace {
            primaries: ColorPrimaries::Bt2020,
            ..ColorSpace::default()
        };
        let overridden = ColorSpace {
            matrix: Some(MatrixCoefficients::Bt709),
            ..bt2020
        };

        assert_eq!(bt2020.matrix_coefficients(), MatrixCoefficients::Bt2020Ncl);
        assert_eq!(overridden.matrix_coefficients(), MatrixCoefficients::Bt709);
    }

    #[test]
    fn luma_rows_sum_to_one_and_chroma_rows_to_zero() {
        for matrix in MatrixCoefficients::ALL {
            let rows = matrix.rows();

            assert!((rows[0].iter().sum::<f64>() - 1.0).abs() < 1e-12);
            assert!(rows[1].iter().sum::<f64>().abs() < 1e-12);
            assert!(rows[2].iter().sum::<f64>().abs() < 1e-12);
        }
    }
}
