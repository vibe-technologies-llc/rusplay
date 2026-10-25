use std::fmt;

use serde::{Deserialize, Serialize};

use crate::color::{
    ColorError,
    matrix::{Matrix3, bradford_adaptation},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "[f64; 2]", into = "[f64; 2]")]
pub struct Chromaticity {
    x: u16,
    y: u16,
}

impl Chromaticity {
    pub const UNITS_PER_ONE: u16 = 50_000;
    pub const D65: Self = Self::units(15_635, 16_450);
    pub const DCI: Self = Self::units(15_700, 17_550);

    const fn units(x: u16, y: u16) -> Self {
        Self { x, y }
    }

    pub const fn from_units(x: u16, y: u16) -> Option<Self> {
        if x > Self::UNITS_PER_ONE || y > Self::UNITS_PER_ONE || y == 0 {
            None
        } else {
            Some(Self { x, y })
        }
    }

    pub fn from_xy(x: f64, y: f64) -> Result<Self, ColorError> {
        let out_of_range = ColorError::ChromaticityOutOfRange { x, y };
        let to_units = |value: f64| {
            let scaled = (value * f64::from(Self::UNITS_PER_ONE)).round();

            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let units = scaled as u16;
            (0.0..=f64::from(Self::UNITS_PER_ONE))
                .contains(&scaled)
                .then_some(units)
        };

        match (to_units(x), to_units(y)) {
            (Some(x), Some(y)) => Self::from_units(x, y).ok_or(out_of_range),
            _ => Err(out_of_range),
        }
    }

    pub const fn to_units(self) -> (u16, u16) {
        (self.x, self.y)
    }

    pub fn xy(self) -> (f64, f64) {
        let one = f64::from(Self::UNITS_PER_ONE);

        (f64::from(self.x) / one, f64::from(self.y) / one)
    }

    pub fn xyz(self) -> [f64; 3] {
        let (x, y) = self.xy();

        [x / y, 1.0, (1.0 - x - y) / y]
    }
}

impl TryFrom<[f64; 2]> for Chromaticity {
    type Error = ColorError;

    fn try_from([x, y]: [f64; 2]) -> Result<Self, ColorError> {
        Self::from_xy(x, y)
    }
}

impl From<Chromaticity> for [f64; 2] {
    fn from(chromaticity: Chromaticity) -> Self {
        let (x, y) = chromaticity.xy();
        [x, y]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ChromaticityFields", into = "ChromaticityFields")]
pub struct Chromaticities {
    red: Chromaticity,
    green: Chromaticity,
    blue: Chromaticity,
    white: Chromaticity,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChromaticityFields {
    red: Chromaticity,
    green: Chromaticity,
    blue: Chromaticity,
    white: Chromaticity,
}

impl Chromaticities {
    pub const BT709: Self = Self::named(
        Chromaticity::units(32_000, 16_500),
        Chromaticity::units(15_000, 30_000),
        Chromaticity::units(7_500, 3_000),
        Chromaticity::D65,
    );
    pub const DISPLAY_P3: Self = Self::named(
        Chromaticity::units(34_000, 16_000),
        Chromaticity::units(13_250, 34_500),
        Chromaticity::units(7_500, 3_000),
        Chromaticity::D65,
    );
    pub const DCI_P3: Self = Self::named(
        Chromaticity::units(34_000, 16_000),
        Chromaticity::units(13_250, 34_500),
        Chromaticity::units(7_500, 3_000),
        Chromaticity::DCI,
    );
    pub const BT2020: Self = Self::named(
        Chromaticity::units(35_400, 14_600),
        Chromaticity::units(8_500, 39_850),
        Chromaticity::units(6_550, 2_300),
        Chromaticity::D65,
    );
    pub const BT601_525: Self = Self::named(
        Chromaticity::units(31_500, 17_000),
        Chromaticity::units(15_500, 29_750),
        Chromaticity::units(7_750, 3_500),
        Chromaticity::D65,
    );
    pub const BT601_625: Self = Self::named(
        Chromaticity::units(32_000, 16_500),
        Chromaticity::units(14_500, 30_000),
        Chromaticity::units(7_500, 3_000),
        Chromaticity::D65,
    );

    const fn named(
        red: Chromaticity,
        green: Chromaticity,
        blue: Chromaticity,
        white: Chromaticity,
    ) -> Self {
        Self {
            red,
            green,
            blue,
            white,
        }
    }

    pub fn new(
        red: Chromaticity,
        green: Chromaticity,
        blue: Chromaticity,
        white: Chromaticity,
    ) -> Result<Self, ColorError> {
        let candidate = Self::named(red, green, blue, white);
        let primaries = candidate.primary_matrix();
        let spans_white = primaries
            .inverse()
            .map(|inverse| inverse.apply(white.xyz()))
            .is_some_and(|scales| scales.iter().all(|scale| scale.is_finite() && *scale > 0.0));

        if spans_white {
            Ok(candidate)
        } else {
            Err(ColorError::DegenerateGamut)
        }
    }

    pub const fn red(&self) -> Chromaticity {
        self.red
    }

    pub const fn green(&self) -> Chromaticity {
        self.green
    }

    pub const fn blue(&self) -> Chromaticity {
        self.blue
    }

    pub const fn white(&self) -> Chromaticity {
        self.white
    }

    fn primary_matrix(&self) -> Matrix3 {
        Matrix3::from_columns([self.red.xyz(), self.green.xyz(), self.blue.xyz()])
    }

    pub fn rgb_to_xyz(&self) -> Matrix3 {
        let primaries = self.primary_matrix();
        let scales = primaries.inverse_of_invertible().apply(self.white.xyz());

        primaries * Matrix3::diagonal(scales)
    }

    pub fn xyz_to_rgb(&self) -> Matrix3 {
        self.rgb_to_xyz().inverse_of_invertible()
    }

    pub fn rgb_to_rgb(&self, target: &Self, adaptation: WhiteAdaptation) -> Matrix3 {
        let xyz = match adaptation {
            WhiteAdaptation::Bradford => {
                bradford_adaptation(self.white.xyz(), target.white.xyz()) * self.rgb_to_xyz()
            }
            WhiteAdaptation::None => self.rgb_to_xyz(),
        };

        target.xyz_to_rgb() * xyz
    }
}

impl TryFrom<ChromaticityFields> for Chromaticities {
    type Error = ColorError;

    fn try_from(fields: ChromaticityFields) -> Result<Self, ColorError> {
        Self::new(fields.red, fields.green, fields.blue, fields.white)
    }
}

impl From<Chromaticities> for ChromaticityFields {
    fn from(chromaticities: Chromaticities) -> Self {
        Self {
            red: chromaticities.red,
            green: chromaticities.green,
            blue: chromaticities.blue,
            white: chromaticities.white,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WhiteAdaptation {
    Bradford,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorPrimaries {
    #[default]
    Bt709,
    DisplayP3,
    DciP3,
    Bt2020,
    #[serde(rename = "bt601-525")]
    Bt601_525,
    #[serde(rename = "bt601-625")]
    Bt601_625,
    Custom(Chromaticities),
}

impl ColorPrimaries {
    pub const NAMED: [Self; 6] = [
        Self::Bt709,
        Self::DisplayP3,
        Self::DciP3,
        Self::Bt2020,
        Self::Bt601_525,
        Self::Bt601_625,
    ];

    pub const fn chromaticities(self) -> Chromaticities {
        match self {
            Self::Bt709 => Chromaticities::BT709,
            Self::DisplayP3 => Chromaticities::DISPLAY_P3,
            Self::DciP3 => Chromaticities::DCI_P3,
            Self::Bt2020 => Chromaticities::BT2020,
            Self::Bt601_525 => Chromaticities::BT601_525,
            Self::Bt601_625 => Chromaticities::BT601_625,
            Self::Custom(chromaticities) => chromaticities,
        }
    }

    pub const fn h273_code(self) -> Option<u8> {
        match self {
            Self::Bt709 => Some(1),
            Self::Bt601_625 => Some(5),
            Self::Bt601_525 => Some(6),
            Self::Bt2020 => Some(9),
            Self::DciP3 => Some(11),
            Self::DisplayP3 => Some(12),
            Self::Custom(_) => None,
        }
    }

    pub fn from_h273_code(code: u8) -> Option<Self> {
        Self::NAMED
            .into_iter()
            .find(|primaries| primaries.h273_code() == Some(code))
    }

    pub fn from_chromaticities(chromaticities: Chromaticities) -> Self {
        Self::NAMED
            .into_iter()
            .find(|named| named.chromaticities() == chromaticities)
            .unwrap_or(Self::Custom(chromaticities))
    }

    pub fn rgb_to_xyz(self) -> Matrix3 {
        self.chromaticities().rgb_to_xyz()
    }

    pub fn rgb_to_rgb(self, target: Self, adaptation: WhiteAdaptation) -> Matrix3 {
        self.chromaticities()
            .rgb_to_rgb(&target.chromaticities(), adaptation)
    }
}

impl fmt::Display for ColorPrimaries {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bt709 => "bt709",
            Self::DisplayP3 => "display-p3",
            Self::DciP3 => "dci-p3",
            Self::Bt2020 => "bt2020",
            Self::Bt601_525 => "bt601-525",
            Self::Bt601_625 => "bt601-625",
            Self::Custom(_) => "custom",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOLERANCE: f64 = 2e-4;

    fn assert_rows_close(got: Matrix3, want: [[f64; 3]; 3]) {
        assert!(
            got.max_difference(&Matrix3(want)) < TOLERANCE,
            "{got:?} != {want:?}"
        );
    }

    #[test]
    fn bt709_rgb_to_xyz_matches_the_published_matrix() {
        assert_rows_close(
            ColorPrimaries::Bt709.rgb_to_xyz(),
            [
                [0.4124, 0.3576, 0.1805],
                [0.2126, 0.7152, 0.0722],
                [0.0193, 0.1192, 0.9505],
            ],
        );
    }

    #[test]
    fn bt2020_luma_row_matches_its_matrix_coefficients() {
        let luma = ColorPrimaries::Bt2020.rgb_to_xyz().rows()[1];

        assert!((luma[0] - 0.2627).abs() < TOLERANCE);
        assert!((luma[2] - 0.0593).abs() < TOLERANCE);
    }

    #[test]
    fn bt2020_to_bt709_matches_bt2087() {
        assert_rows_close(
            ColorPrimaries::Bt2020.rgb_to_rgb(ColorPrimaries::Bt709, WhiteAdaptation::Bradford),
            [
                [1.6605, -0.5876, -0.0728],
                [-0.1246, 1.1329, -0.0083],
                [-0.0182, -0.1006, 1.1187],
            ],
        );
    }

    #[test]
    fn every_named_white_maps_to_unit_y() {
        for primaries in ColorPrimaries::NAMED {
            let white = primaries.rgb_to_xyz().apply([1.0; 3]);

            assert!((white[1] - 1.0).abs() < 1e-12, "{primaries}");
        }
    }

    #[test]
    fn adaptation_maps_white_to_white_across_white_points() {
        let matrix =
            ColorPrimaries::DciP3.rgb_to_rgb(ColorPrimaries::DisplayP3, WhiteAdaptation::Bradford);

        for channel in matrix.apply([1.0; 3]) {
            assert!((channel - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn h273_codes_round_trip() {
        for primaries in ColorPrimaries::NAMED {
            let code = primaries.h273_code().unwrap();

            assert_eq!(ColorPrimaries::from_h273_code(code), Some(primaries));
        }
    }

    #[test]
    fn chromaticities_resolve_to_the_named_primaries_first() {
        let custom = Chromaticities::new(
            Chromaticity::from_xy(0.68, 0.32).unwrap(),
            Chromaticity::from_xy(0.265, 0.69).unwrap(),
            Chromaticity::from_xy(0.15, 0.06).unwrap(),
            Chromaticity::from_xy(0.3, 0.33).unwrap(),
        )
        .unwrap();

        for primaries in ColorPrimaries::NAMED {
            assert_eq!(
                ColorPrimaries::from_chromaticities(primaries.chromaticities()),
                primaries
            );
        }
        assert_eq!(
            ColorPrimaries::from_chromaticities(custom),
            ColorPrimaries::Custom(custom)
        );
    }

    #[test]
    fn chromaticity_units_are_st2086_steps() {
        let chromaticity = Chromaticity::from_xy(0.708, 0.292).unwrap();

        assert_eq!(chromaticity.to_units(), (35_400, 14_600));
        assert_eq!(<[f64; 2]>::from(chromaticity), [0.708, 0.292]);
    }

    #[test]
    fn out_of_range_chromaticities_are_rejected() {
        assert!(Chromaticity::from_xy(-0.1, 0.3).is_err());
        assert!(Chromaticity::from_xy(0.3, 0.0).is_err());
        assert!(Chromaticity::from_xy(0.3, 1.5).is_err());
    }

    #[test]
    fn collinear_primaries_are_rejected() {
        let point = |x, y| Chromaticity::from_xy(x, y).unwrap();

        let collinear = Chromaticities::new(
            point(0.1, 0.1),
            point(0.2, 0.2),
            point(0.3, 0.3),
            Chromaticity::D65,
        );

        assert_eq!(collinear, Err(ColorError::DegenerateGamut));
    }

    #[test]
    fn white_outside_the_triangle_is_rejected() {
        let bt709 = Chromaticities::BT709;

        let outside = Chromaticities::new(
            bt709.red(),
            bt709.green(),
            bt709.blue(),
            Chromaticity::from_xy(0.1, 0.8).unwrap(),
        );

        assert_eq!(outside, Err(ColorError::DegenerateGamut));
    }

    #[test]
    fn custom_matching_named_primaries_gives_the_same_matrix() {
        let bt2020 = Chromaticities::BT2020;
        let custom =
            Chromaticities::new(bt2020.red(), bt2020.green(), bt2020.blue(), bt2020.white())
                .unwrap();

        assert_eq!(
            ColorPrimaries::Custom(custom).rgb_to_xyz(),
            ColorPrimaries::Bt2020.rgb_to_xyz()
        );
    }
}
