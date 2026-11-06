use rusplay_core::{Chromaticity, ContentLightLevel, Luminance, MasteringDisplay, SignalledColor};

use crate::metadata::{Primaries, content_light, mastering_display};

#[derive(Clone, Copy)]
pub(super) struct Rational {
    pub num: i32,
    pub den: i32,
}

pub(super) fn h273_code(code: u32) -> u8 {
    u8::try_from(code).unwrap_or(2)
}

pub(super) fn vui_from_location(location: u32) -> Option<u32> {
    location.checked_sub(1)
}

pub(super) fn with_range(color: SignalledColor, full: Option<bool>) -> SignalledColor {
    match full {
        Some(full) => color.with_full_range(full),
        None => color,
    }
}

pub(super) fn with_location(color: SignalledColor, location: u32) -> SignalledColor {
    match vui_from_location(location) {
        Some(vui) => color.with_vui_chroma_location(vui),
        None => color,
    }
}

pub(super) fn mastering_from_rationals(
    red: [Rational; 2],
    green: [Rational; 2],
    blue: [Rational; 2],
    white: [Rational; 2],
    max_luminance: Rational,
    min_luminance: Rational,
) -> Option<MasteringDisplay> {
    let point = |value: Rational| chromaticity_units(value);
    let xy = |pair: [Rational; 2]| Some([point(pair[0])?, point(pair[1])?]);

    mastering_display(
        Primaries {
            red: xy(red)?,
            green: xy(green)?,
            blue: xy(blue)?,
            white: xy(white)?,
        },
        luminance_units(max_luminance)?,
        luminance_units(min_luminance)?,
    )
}

pub(super) fn content_from_levels(max_cll: u32, max_fall: u32) -> ContentLightLevel {
    content_light(clamp_u16(max_cll), clamp_u16(max_fall))
}

fn chromaticity_units(value: Rational) -> Option<u16> {
    let units = rational_units(value.num, value.den, u32::from(Chromaticity::UNITS_PER_ONE))?;

    u16::try_from(units).ok()
}

fn luminance_units(value: Rational) -> Option<u32> {
    rational_units(value.num, value.den, Luminance::UNITS_PER_NIT)
}

fn rational_units(num: i32, den: i32, units_per_one: u32) -> Option<u32> {
    if den <= 0 || num < 0 {
        return None;
    }

    let numerator = u64::from(u32::try_from(num).ok()?);
    let denominator = u64::from(u32::try_from(den).ok()?);
    let units = u64::from(units_per_one);
    let scaled = numerator.checked_mul(units)?.checked_add(denominator / 2)? / denominator;

    u32::try_from(scaled).ok()
}

fn clamp_u16(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ChromaSiting, ColorRange};

    use super::*;

    fn ratio(num: i32, den: i32) -> Rational {
        Rational { num, den }
    }

    #[test]
    fn rationals_round_to_st2086_units_and_reject_a_bad_fraction() {
        assert_eq!(rational_units(1, 2, 50_000), Some(25_000));
        assert_eq!(rational_units(1, 3, 50_000), Some(16_667));
        assert_eq!(rational_units(1_000, 1, 10_000), Some(10_000_000));
        assert_eq!(rational_units(-1, 2, 50_000), None);
        assert_eq!(rational_units(1, 0, 50_000), None);
        assert_eq!(rational_units(1, -2, 50_000), None);
    }

    #[test]
    fn mastering_metadata_uses_rgb_order_and_drops_a_degenerate_gamut() {
        let display = mastering_from_rationals(
            [ratio(34_000, 50_000), ratio(16_000, 50_000)],
            [ratio(15_000, 50_000), ratio(33_000, 50_000)],
            [ratio(7_500, 50_000), ratio(3_000, 50_000)],
            [ratio(15_629, 50_000), ratio(16_500, 50_000)],
            ratio(1_000, 1),
            ratio(1, 10_000),
        )
        .unwrap();

        assert_eq!(
            display.max_nits.units(),
            1_000 * rusplay_core::Luminance::UNITS_PER_NIT
        );
        assert!(
            mastering_from_rationals(
                [ratio(1, 2), ratio(1, 2)],
                [ratio(1, 2), ratio(1, 2)],
                [ratio(1, 2), ratio(1, 2)],
                [ratio(1, 3), ratio(1, 3)],
                ratio(1_000, 1),
                ratio(1, 10_000),
            )
            .is_none()
        );
    }

    #[test]
    fn content_light_saturates_to_the_level_width() {
        let light = content_from_levels(u32::MAX, 0);

        assert_eq!(light.max_cll_nits, u16::MAX);
        assert_eq!(light.max_fall_nits, 0);
    }

    #[test]
    fn h273_codes_above_a_byte_are_unspecified() {
        assert_eq!(h273_code(1), 1);
        assert_eq!(h273_code(2), 2);
        assert_eq!(h273_code(300), 2);
    }

    #[test]
    fn chroma_location_follows_the_vui_types() {
        let color = SignalledColor::default();

        assert_eq!(with_location(color, 0).chroma_siting, None);
        assert_eq!(
            with_location(color, 1).chroma_siting,
            Some(ChromaSiting::Left)
        );
        assert_eq!(
            with_location(color, 2).chroma_siting,
            Some(ChromaSiting::Center)
        );
        assert_eq!(with_location(color, 7).chroma_siting, None);
    }

    #[test]
    fn an_unset_range_stays_unset_until_a_side_says_otherwise() {
        let color = SignalledColor::default();

        assert_eq!(with_range(color, None).range, None);
        assert_eq!(
            with_range(color, Some(false)).range,
            Some(ColorRange::Limited)
        );
        assert_eq!(with_range(color, Some(true)).range, Some(ColorRange::Full));
    }
}
