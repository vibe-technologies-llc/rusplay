mod wayland;

use std::ops::RangeInclusive;

use rusplay_config::DisplayTargetConfig;
use rusplay_core::{
    Chromaticities, Chromaticity, ColorPrimaries, HdrLevels, Luminance, TransferFunction,
};
use rusplay_scale::ColorVolume;
use wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::{
    Primaries as WireNamedPrimaries, TransferFunction as WireNamedTransfer,
};

use crate::ClientError;
pub use crate::display::wayland::{OutputDisplay, query_display, query_outputs, query_target};

const MIN_LUMINANCE_UNITS_PER_NIT: f64 = 10_000.0;
const WIRE_CHROMATICITY_UNITS_PER_ONE: f64 = 1_000_000.0;
const POWER_CURVE_UNITS_PER_ONE: f64 = 10_000.0;
const GAMMA22_RANGE: RangeInclusive<f64> = 2.1..=2.3;
const NITS_RANGE: RangeInclusive<f64> = 1.0..=HdrLevels::MAX_NITS as f64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Luminances {
    pub min_nits: f64,
    pub max_nits: f64,
    pub reference_nits: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LuminanceRange {
    pub min_nits: f64,
    pub max_nits: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DisplayImage {
    pub primaries: Option<ColorPrimaries>,
    pub target_primaries: Option<ColorPrimaries>,
    pub transfer: Option<TransferFunction>,
    pub luminances: Option<Luminances>,
    pub target_luminance: Option<LuminanceRange>,
}

pub fn resolve_target(
    image: &DisplayImage,
    overrides: &DisplayTargetConfig,
) -> Result<ColorVolume, ClientError> {
    let transfer = overrides
        .transfer
        .or(image.transfer)
        .unwrap_or(TransferFunction::Srgb);
    let primaries = overrides
        .primaries
        .or(image.target_primaries)
        .or(image.primaries)
        .unwrap_or(ColorPrimaries::Bt709);
    let reference_white_nits = overrides
        .reference_white_nits
        .map(f64::from)
        .or(image.luminances.map(|levels| levels.reference_nits))
        .unwrap_or(ColorVolume::SDR_REFERENCE_WHITE_NITS);
    let peak_nits = overrides
        .peak_nits
        .map(f64::from)
        .or(image.target_luminance.map(|range| range.max_nits))
        .or(image.luminances.map(|levels| levels.max_nits))
        .unwrap_or_else(|| default_peak_nits(transfer, reference_white_nits));
    let black_nits = overrides
        .black_nits
        .map(Luminance::nits)
        .or(image.target_luminance.map(|range| range.min_nits))
        .or(image.luminances.map(|levels| levels.min_nits))
        .unwrap_or(0.0);

    if black_nits >= peak_nits {
        return Err(ClientError::TargetBlackNotBelowPeak {
            black_nits,
            peak_nits,
        });
    }

    Ok(ColorVolume {
        primaries,
        transfer,
        reference_white_nits,
        peak_nits,
        black_nits,
    })
}

fn default_peak_nits(transfer: TransferFunction, reference_white_nits: f64) -> f64 {
    if transfer.is_hdr() {
        f64::from(HdrLevels::default().peak_nits)
    } else {
        reference_white_nits
    }
}

pub(crate) fn primaries_from_chromaticity(values: [i32; 8]) -> Option<ColorPrimaries> {
    let point = |x: i32, y: i32| {
        Chromaticity::from_xy(
            f64::from(x) / WIRE_CHROMATICITY_UNITS_PER_ONE,
            f64::from(y) / WIRE_CHROMATICITY_UNITS_PER_ONE,
        )
        .ok()
    };
    let [
        red_x,
        red_y,
        green_x,
        green_y,
        blue_x,
        blue_y,
        white_x,
        white_y,
    ] = values;

    let chromaticities = Chromaticities::new(
        point(red_x, red_y)?,
        point(green_x, green_y)?,
        point(blue_x, blue_y)?,
        point(white_x, white_y)?,
    )
    .ok()?;

    Some(
        ColorPrimaries::NAMED
            .into_iter()
            .find(|named| named.chromaticities() == chromaticities)
            .unwrap_or(ColorPrimaries::Custom(chromaticities)),
    )
}

pub(crate) fn primaries_from_named(named: WireNamedPrimaries) -> Option<ColorPrimaries> {
    match named {
        WireNamedPrimaries::Srgb => Some(ColorPrimaries::Bt709),
        WireNamedPrimaries::Pal => Some(ColorPrimaries::Bt601_625),
        WireNamedPrimaries::Ntsc => Some(ColorPrimaries::Bt601_525),
        WireNamedPrimaries::Bt2020 => Some(ColorPrimaries::Bt2020),
        WireNamedPrimaries::DciP3 => Some(ColorPrimaries::DciP3),
        WireNamedPrimaries::DisplayP3 => Some(ColorPrimaries::DisplayP3),
        WireNamedPrimaries::PalM => Some(ColorPrimaries::Custom(Chromaticities::BT470M)),
        WireNamedPrimaries::GenericFilm => {
            Some(ColorPrimaries::Custom(Chromaticities::GENERIC_FILM))
        }
        WireNamedPrimaries::AdobeRgb => Some(ColorPrimaries::Custom(Chromaticities::ADOBE_RGB)),
        _ => None,
    }
}

pub(crate) fn transfer_from_named(named: WireNamedTransfer) -> Option<TransferFunction> {
    match named {
        WireNamedTransfer::Bt1886 => Some(TransferFunction::Bt709),
        WireNamedTransfer::Gamma22 => Some(TransferFunction::Gamma22),
        WireNamedTransfer::ExtLinear => Some(TransferFunction::Linear),
        WireNamedTransfer::Srgb
        | WireNamedTransfer::ExtSrgb
        | WireNamedTransfer::CompoundPower24 => Some(TransferFunction::Srgb),
        WireNamedTransfer::St2084Pq => Some(TransferFunction::Pq),
        WireNamedTransfer::Hlg => Some(TransferFunction::Hlg),
        _ => None,
    }
}

pub(crate) fn transfer_from_power(exponent_units: u32) -> Option<TransferFunction> {
    let exponent = f64::from(exponent_units) / POWER_CURVE_UNITS_PER_ONE;

    GAMMA22_RANGE
        .contains(&exponent)
        .then_some(TransferFunction::Gamma22)
}

pub(crate) fn luminances_from_wire(
    min_units: u32,
    max_nits: u32,
    reference_nits: u32,
) -> Option<Luminances> {
    let levels = Luminances {
        min_nits: f64::from(min_units) / MIN_LUMINANCE_UNITS_PER_NIT,
        max_nits: f64::from(max_nits),
        reference_nits: f64::from(reference_nits),
    };

    (NITS_RANGE.contains(&levels.max_nits)
        && NITS_RANGE.contains(&levels.reference_nits)
        && levels.min_nits < levels.max_nits)
        .then_some(levels)
}

pub(crate) fn target_luminance_from_wire(min_units: u32, max_nits: u32) -> Option<LuminanceRange> {
    let range = LuminanceRange {
        min_nits: f64::from(min_units) / MIN_LUMINANCE_UNITS_PER_NIT,
        max_nits: f64::from(max_nits),
    };

    (NITS_RANGE.contains(&range.max_nits) && range.min_nits < range.max_nits).then_some(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr_display() -> DisplayImage {
        DisplayImage {
            primaries: Some(ColorPrimaries::Bt2020),
            target_primaries: Some(ColorPrimaries::DisplayP3),
            transfer: Some(TransferFunction::Pq),
            luminances: Some(Luminances {
                min_nits: 0.0,
                max_nits: 10_000.0,
                reference_nits: 203.0,
            }),
            target_luminance: Some(LuminanceRange {
                min_nits: 0.0005,
                max_nits: 1000.0,
            }),
        }
    }

    fn sdr_display() -> DisplayImage {
        DisplayImage {
            primaries: Some(ColorPrimaries::Bt709),
            target_primaries: Some(ColorPrimaries::Bt709),
            transfer: Some(TransferFunction::Gamma22),
            luminances: Some(Luminances {
                min_nits: 0.2,
                max_nits: 250.0,
                reference_nits: 80.0,
            }),
            target_luminance: None,
        }
    }

    #[test]
    fn nothing_reported_and_nothing_overridden_is_the_srgb_display() {
        let target = resolve_target(&DisplayImage::default(), &DisplayTargetConfig::default());

        assert_eq!(target.unwrap(), ColorVolume::srgb_display());
    }

    #[test]
    fn an_hdr_output_resolves_to_its_target_volume() {
        let target = resolve_target(&hdr_display(), &DisplayTargetConfig::default()).unwrap();

        assert_eq!(target.primaries, ColorPrimaries::DisplayP3);
        assert_eq!(target.transfer, TransferFunction::Pq);
        assert!((target.peak_nits - 1000.0).abs() < f64::EPSILON);
        assert!((target.black_nits - 0.0005).abs() < 1e-12);
        assert!((target.reference_white_nits - 203.0).abs() < f64::EPSILON);
        assert!(target.is_hdr());
    }

    #[test]
    fn an_sdr_output_keeps_its_reported_white_and_gamma() {
        let target = resolve_target(&sdr_display(), &DisplayTargetConfig::default()).unwrap();

        assert_eq!(target.transfer, TransferFunction::Gamma22);
        assert!((target.reference_white_nits - 80.0).abs() < f64::EPSILON);
        assert!((target.peak_nits - 250.0).abs() < f64::EPSILON);
        assert!((target.black_nits - 0.2).abs() < 1e-12);
        assert!(!target.is_hdr());
    }

    #[test]
    fn overrides_win_field_by_field() {
        let overrides = DisplayTargetConfig {
            primaries: Some(ColorPrimaries::Bt2020),
            reference_white_nits: Some(160),
            peak_nits: Some(600),
            ..DisplayTargetConfig::default()
        };

        let target = resolve_target(&hdr_display(), &overrides).unwrap();

        assert_eq!(target.primaries, ColorPrimaries::Bt2020);
        assert_eq!(target.transfer, TransferFunction::Pq);
        assert!((target.reference_white_nits - 160.0).abs() < f64::EPSILON);
        assert!((target.peak_nits - 600.0).abs() < f64::EPSILON);
        assert!((target.black_nits - 0.0005).abs() < 1e-12);
    }

    #[test]
    fn overrides_alone_describe_a_display_that_reports_nothing() {
        let overrides = DisplayTargetConfig {
            primaries: Some(ColorPrimaries::DisplayP3),
            transfer: Some(TransferFunction::Pq),
            peak_nits: Some(800),
            black_nits: Some(Luminance::from_nits(0.01).unwrap()),
            ..DisplayTargetConfig::default()
        };

        let target = resolve_target(&DisplayImage::default(), &overrides).unwrap();

        assert_eq!(target.primaries, ColorPrimaries::DisplayP3);
        assert!(target.is_hdr());
        assert!((target.peak_nits - 800.0).abs() < f64::EPSILON);
        assert!((target.black_nits - 0.01).abs() < 1e-12);
        assert!((target.reference_white_nits - 203.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_hdr_transfer_override_without_levels_assumes_the_default_peak() {
        let overrides = DisplayTargetConfig {
            transfer: Some(TransferFunction::Hlg),
            ..DisplayTargetConfig::default()
        };

        let target = resolve_target(&DisplayImage::default(), &overrides).unwrap();

        assert!((target.peak_nits - f64::from(HdrLevels::default().peak_nits)).abs() < 1e-9);
    }

    #[test]
    fn the_primary_volume_is_used_when_no_target_volume_is_reported() {
        let image = DisplayImage {
            target_primaries: None,
            ..hdr_display()
        };

        let target = resolve_target(&image, &DisplayTargetConfig::default()).unwrap();

        assert_eq!(target.primaries, ColorPrimaries::Bt2020);
    }

    #[test]
    fn a_black_level_at_or_above_the_peak_is_an_error() {
        let overrides = DisplayTargetConfig {
            peak_nits: Some(1),
            ..DisplayTargetConfig::default()
        };
        let image = DisplayImage {
            target_luminance: Some(LuminanceRange {
                min_nits: 2.0,
                max_nits: 100.0,
            }),
            ..hdr_display()
        };

        assert!(matches!(
            resolve_target(&image, &overrides),
            Err(ClientError::TargetBlackNotBelowPeak { .. })
        ));
    }

    #[test]
    fn wire_chromaticities_match_the_named_sets() {
        let bt2020 = [
            708_000, 292_000, 170_000, 797_000, 131_000, 46_000, 312_700, 329_000,
        ];
        let p3 = [
            680_000, 320_000, 265_000, 690_000, 150_000, 60_000, 312_700, 329_000,
        ];

        assert_eq!(
            primaries_from_chromaticity(bt2020),
            Some(ColorPrimaries::Bt2020)
        );
        assert_eq!(
            primaries_from_chromaticity(p3),
            Some(ColorPrimaries::DisplayP3)
        );
    }

    #[test]
    fn measured_chromaticities_become_custom_primaries() {
        let measured = [
            678_000, 321_000, 271_000, 683_000, 148_000, 62_000, 312_700, 329_000,
        ];

        let primaries = primaries_from_chromaticity(measured);

        assert!(matches!(primaries, Some(ColorPrimaries::Custom(_))));
    }

    #[test]
    fn degenerate_wire_chromaticities_are_not_reported() {
        let collinear = [
            100_000, 100_000, 200_000, 200_000, 300_000, 300_000, 312_700, 329_000,
        ];
        let zero_y = [0, 0, 0, 0, 0, 0, 0, 0];

        assert_eq!(primaries_from_chromaticity(collinear), None);
        assert_eq!(primaries_from_chromaticity(zero_y), None);
    }

    #[test]
    fn named_primaries_map_to_the_model() {
        assert_eq!(
            primaries_from_named(WireNamedPrimaries::Srgb),
            Some(ColorPrimaries::Bt709)
        );
        assert_eq!(
            primaries_from_named(WireNamedPrimaries::DisplayP3),
            Some(ColorPrimaries::DisplayP3)
        );
        assert_eq!(
            primaries_from_named(WireNamedPrimaries::AdobeRgb),
            Some(ColorPrimaries::Custom(Chromaticities::ADOBE_RGB))
        );
        assert_eq!(primaries_from_named(WireNamedPrimaries::Cie1931Xyz), None);
    }

    #[test]
    fn named_transfers_map_to_the_model() {
        assert_eq!(
            transfer_from_named(WireNamedTransfer::St2084Pq),
            Some(TransferFunction::Pq)
        );
        assert_eq!(
            transfer_from_named(WireNamedTransfer::CompoundPower24),
            Some(TransferFunction::Srgb)
        );
        assert_eq!(
            transfer_from_named(WireNamedTransfer::ExtLinear),
            Some(TransferFunction::Linear)
        );
        assert_eq!(transfer_from_named(WireNamedTransfer::Log100), None);
    }

    #[test]
    fn a_power_curve_near_2_2_is_gamma_22() {
        assert_eq!(transfer_from_power(22_000), Some(TransferFunction::Gamma22));
        assert_eq!(transfer_from_power(24_000), None);
    }

    #[test]
    fn wire_luminances_are_scaled_and_range_checked() {
        let levels = luminances_from_wire(2_000, 250, 80).unwrap();

        assert!((levels.min_nits - 0.2).abs() < 1e-12);
        assert!((levels.max_nits - 250.0).abs() < f64::EPSILON);
        assert!((levels.reference_nits - 80.0).abs() < f64::EPSILON);
        assert_eq!(luminances_from_wire(0, 0, 80), None);
        assert_eq!(luminances_from_wire(0, 20_000, 80), None);
        assert_eq!(luminances_from_wire(5_000_000, 250, 80), None);
    }

    #[test]
    fn wire_target_luminance_needs_an_ordered_range() {
        let range = target_luminance_from_wire(5, 1000).unwrap();

        assert!((range.min_nits - 0.0005).abs() < 1e-12);
        assert_eq!(target_luminance_from_wire(0, 0), None);
    }
}
