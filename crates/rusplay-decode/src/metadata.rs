use rusplay_core::{
    Chromaticities, Chromaticity, ColorPrimaries, ContentLightLevel, Luminance, MasteringDisplay,
};

const AV1_CHROMATICITY_UNITS: u64 = 1 << 16;
const AV1_MAX_LUMINANCE_UNITS: u64 = 1 << 8;
const AV1_MIN_LUMINANCE_UNITS: u64 = 1 << 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Primaries {
    pub red: [u16; 2],
    pub green: [u16; 2],
    pub blue: [u16; 2],
    pub white: [u16; 2],
}

pub(crate) fn mastering_display(
    primaries: Primaries,
    max_units: u32,
    min_units: u32,
) -> Option<MasteringDisplay> {
    let point = |[x, y]: [u16; 2]| Chromaticity::from_units(x, y);
    let chromaticities = Chromaticities::new(
        point(primaries.red)?,
        point(primaries.green)?,
        point(primaries.blue)?,
        point(primaries.white)?,
    )
    .ok()?;

    Some(MasteringDisplay {
        primaries: ColorPrimaries::from_chromaticities(chromaticities),
        max_nits: Luminance::from_units(max_units)?,
        min_nits: Luminance::from_units(min_units)?,
    })
}

pub(crate) fn av1_mastering_display(
    primaries: [[u16; 2]; 3],
    white: [u16; 2],
    max_luminance: u32,
    min_luminance: u32,
) -> Option<MasteringDisplay> {
    let chromaticity = |[x, y]: [u16; 2]| {
        [x, y].map(|value| {
            rescale(
                u64::from(value),
                u64::from(Chromaticity::UNITS_PER_ONE),
                AV1_CHROMATICITY_UNITS,
            )
        })
    };
    let luminance = |value: u32, units: u64| {
        rescale(u64::from(value), u64::from(Luminance::UNITS_PER_NIT), units)
    };

    mastering_display(
        Primaries {
            red: chromaticity(primaries[0]).map(saturate_u16),
            green: chromaticity(primaries[1]).map(saturate_u16),
            blue: chromaticity(primaries[2]).map(saturate_u16),
            white: chromaticity(white).map(saturate_u16),
        },
        saturate_u32(luminance(max_luminance, AV1_MAX_LUMINANCE_UNITS)),
        saturate_u32(luminance(min_luminance, AV1_MIN_LUMINANCE_UNITS)),
    )
}

pub(crate) const fn content_light(max_cll: u16, max_fall: u16) -> ContentLightLevel {
    ContentLightLevel {
        max_cll_nits: max_cll,
        max_fall_nits: max_fall,
    }
}

const fn rescale(value: u64, to: u64, from: u64) -> u64 {
    (value * to + from / 2) / from
}

fn saturate_u16(value: u64) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn saturate_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn av1_fixed_point_maps_onto_st2086_units() {
        let display = av1_mastering_display(
            [[45_875, 21_627], [8_847, 45_220], [4_260, 1_507]],
            [20_493, 21_561],
            1000 << 8,
            82,
        )
        .unwrap();

        assert!(matches!(display.primaries, ColorPrimaries::Custom(_)));
        assert_eq!(display.max_nits, Luminance::from_whole_nits(1000).unwrap());
        assert_eq!(display.min_nits.units(), 50);
    }

    #[test]
    fn bt2020_av1_primaries_resolve_to_the_named_set() {
        let to_av1 = |units: u16| {
            u16::try_from(rescale(
                u64::from(units),
                AV1_CHROMATICITY_UNITS,
                u64::from(Chromaticity::UNITS_PER_ONE),
            ))
            .unwrap()
        };
        let bt2020 = Chromaticities::BT2020;
        let av1 = |point: Chromaticity| {
            let (x, y) = point.to_units();
            [to_av1(x), to_av1(y)]
        };

        let display = av1_mastering_display(
            [av1(bt2020.red()), av1(bt2020.green()), av1(bt2020.blue())],
            av1(bt2020.white()),
            4000 << 8,
            1 << 14,
        )
        .unwrap();

        assert_eq!(display.primaries, ColorPrimaries::Bt2020);
        assert_eq!(display.max_nits, Luminance::from_whole_nits(4000).unwrap());
        assert_eq!(display.min_nits, Luminance::from_whole_nits(1).unwrap());
    }

    #[test]
    fn a_degenerate_mastering_display_is_ignored() {
        let flat = Primaries {
            red: [10_000, 10_000],
            green: [20_000, 20_000],
            blue: [30_000, 30_000],
            white: [15_635, 16_450],
        };

        assert_eq!(mastering_display(flat, 10_000_000, 50), None);
    }
}
