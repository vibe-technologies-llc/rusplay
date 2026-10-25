use rusplay_core::{ColorPrimaries, GamutMapMode, Matrix3, TransferFunction};

use super::*;
use crate::{
    color::reference,
    testing::{self, Image, RGBA16F_TOLERANCE},
};

const EXACT: f64 = 1e-9;
const HUE_TOLERANCE_DEGREES: f64 = 1.0;
const MAX_HUE_DELTA_E_ITP: f64 = 1.0;
const DELTA_E_ITP_SCALE: f64 = 720.0;

pub(super) fn sdr(primaries: ColorPrimaries) -> ColorVolume {
    ColorVolume {
        primaries,
        ..ColorVolume::srgb_display()
    }
}

pub(super) fn hdr(primaries: ColorPrimaries, peak_nits: f64) -> ColorVolume {
    ColorVolume {
        primaries,
        transfer: TransferFunction::Pq,
        reference_white_nits: ColorVolume::SDR_REFERENCE_WHITE_NITS,
        peak_nits,
        black_nits: 0.0,
    }
}

fn settings(source: ColorVolume, target: ColorVolume, mode: GamutMapMode) -> ColorMapSettings {
    ColorMapSettings {
        source,
        target,
        gamut: GamutMapping {
            mode,
            ..GamutMapping::default()
        },
        ..ColorMapSettings::default()
    }
}

fn bt2020_to_p3_sdr() -> ColorPlan {
    ColorPlan::new(&settings(
        sdr(ColorPrimaries::Bt2020),
        sdr(ColorPrimaries::DisplayP3),
        GamutMapMode::Perceptual,
    ))
    .unwrap()
}

fn bt2020_to_p3_hdr() -> ColorPlan {
    ColorPlan::new(&settings(
        hdr(ColorPrimaries::Bt2020, 1000.0),
        hdr(ColorPrimaries::DisplayP3, 1000.0),
        GamutMapMode::Perceptual,
    ))
    .unwrap()
}

pub(super) fn max_difference(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f64::max)
}

pub(super) fn polar(lab: [f64; 3]) -> (f64, f64) {
    (lab[1].hypot(lab[2]), lab[2].atan2(lab[1]))
}

pub(super) fn hue_difference_degrees(a: f64, b: f64) -> f64 {
    let turn = std::f64::consts::TAU;
    let difference = (a - b).rem_euclid(turn);

    difference.min(turn - difference).to_degrees()
}

fn hue_delta_e_itp(source: [f64; 3], mapped: [f64; 3]) -> f64 {
    let itp = |lab: [f64; 3]| [lab[1] * 0.5, lab[2]];
    let (source_tp, mapped_tp) = (itp(source), itp(mapped));
    let source_chroma = source_tp[0].hypot(source_tp[1]);
    let mapped_chroma = mapped_tp[0].hypot(mapped_tp[1]);
    let angle = source_tp[1].atan2(source_tp[0]) - mapped_tp[1].atan2(mapped_tp[0]);

    DELTA_E_ITP_SCALE * 2.0 * (source_chroma * mapped_chroma).sqrt() * (angle / 2.0).sin().abs()
}

pub(super) fn primaries_and_secondaries(level: f64) -> [[f64; 3]; 6] {
    [
        [level, 0.0, 0.0],
        [0.0, level, 0.0],
        [0.0, 0.0, level],
        [0.0, level, level],
        [level, 0.0, level],
        [level, level, 0.0],
    ]
}

pub(super) fn unit_hash(index: u32, seed: u32) -> f64 {
    let mut state = index.wrapping_mul(0x9E37_79B1) ^ seed.wrapping_mul(0x85EB_CA77);
    state ^= state >> 15;
    state = state.wrapping_mul(0x2C1B_3C6D);
    state ^= state >> 12;

    f64::from(state % 10_000) / 9_999.0
}

#[test]
fn contained_source_is_a_plain_matrix() {
    let plan = ColorPlan::new(&settings(
        sdr(ColorPrimaries::Bt709),
        sdr(ColorPrimaries::Bt2020),
        GamutMapMode::Perceptual,
    ))
    .unwrap();

    let rgb = [0.9, 0.2, 0.05];

    assert_eq!(plan.path(), MappingPath::Matrix);
    assert!(
        max_difference(
            reference::map(&plan, rgb),
            plan.source_to_target().apply(rgb)
        ) < EXACT
    );
}

#[test]
fn identical_primaries_map_through_the_identity() {
    let plan = ColorPlan::new(&settings(
        sdr(ColorPrimaries::DisplayP3),
        sdr(ColorPrimaries::DisplayP3),
        GamutMapMode::Perceptual,
    ))
    .unwrap();

    assert_eq!(plan.path(), MappingPath::Matrix);
    assert!(plan.source_to_target().max_difference(&Matrix3::IDENTITY) < EXACT);
}

#[test]
fn wider_source_takes_the_mode_path_in_the_right_space() {
    let modes = [
        (GamutMapMode::Perceptual, MappingPath::Perceptual),
        (GamutMapMode::Relative, MappingPath::Clip),
        (GamutMapMode::Absolute, MappingPath::Clip),
        (GamutMapMode::Saturation, MappingPath::Clip),
        (GamutMapMode::Desaturate, MappingPath::Desaturate),
        (GamutMapMode::Highlight, MappingPath::Highlight),
    ];

    for (mode, path) in modes {
        let plan = ColorPlan::new(&settings(
            sdr(ColorPrimaries::Bt2020),
            sdr(ColorPrimaries::DisplayP3),
            mode,
        ))
        .unwrap();

        assert_eq!(plan.path(), path, "{mode}");
    }

    assert_eq!(bt2020_to_p3_sdr().space(), WorkingSpace::Oklab);
    assert_eq!(bt2020_to_p3_hdr().space(), WorkingSpace::Ictcp);
}

#[test]
fn absolute_mode_keeps_the_source_white() {
    let plan = ColorPlan::new(&settings(
        sdr(ColorPrimaries::DciP3),
        sdr(ColorPrimaries::DisplayP3),
        GamutMapMode::Absolute,
    ))
    .unwrap();
    let relative = ColorPlan::new(&settings(
        sdr(ColorPrimaries::DciP3),
        sdr(ColorPrimaries::DisplayP3),
        GamutMapMode::Relative,
    ))
    .unwrap();

    let white = plan.source_to_target().apply([1.0; 3]);
    let adapted = relative.source_to_target().apply([1.0; 3]);

    assert!(max_difference(white, [1.0; 3]) > 0.01);
    assert!(max_difference(adapted, [1.0; 3]) < EXACT);
}

#[test]
fn colours_well_inside_the_target_are_unchanged() {
    for plan in [bt2020_to_p3_sdr(), bt2020_to_p3_hdr()] {
        let to_bt2020 = ColorPrimaries::Bt709.rgb_to_rgb(
            ColorPrimaries::Bt2020,
            rusplay_core::WhiteAdaptation::Bradford,
        );
        let mut checked = 0;

        for index in 0..400 {
            let srgb = [0, 1, 2].map(|channel| 0.05 + 0.9 * unit_hash(index, channel));
            let grey = (srgb[0] + srgb[1] + srgb[2]) / 3.0;
            let muted = srgb.map(|value| grey + 0.5 * (value - grey));
            let source = to_bt2020.apply(muted);

            let [lightness, a, b] = reference::source_lab(&plan, source);
            let chroma = a.hypot(b);
            let hue = [a / chroma, b / chroma];
            let target_limit =
                reference::chroma_boundary(&plan, lightness, hue, &plan.work_to_target, 1.0);
            if chroma >= plan.knee * target_limit {
                continue;
            }

            let mapped = reference::map(&plan, source);
            let direct = plan.source_to_target().apply(source);

            assert!(
                max_difference(mapped, direct) < 1e-6,
                "{source:?}: {mapped:?} != {direct:?}"
            );
            checked += 1;
        }

        assert!(checked > 300, "only {checked} colours were well inside");
    }
}

#[test]
fn greys_pass_through_unchanged() {
    for plan in [bt2020_to_p3_sdr(), bt2020_to_p3_hdr()] {
        for level in [0.0, 0.01, 0.18, 0.5, 1.0] {
            let mapped = reference::map(&plan, [level; 3]);

            assert!(
                max_difference(mapped, [level; 3]) < 1e-6,
                "{level}: {mapped:?}"
            );
        }
    }
}

#[test]
fn bt2020_primaries_land_on_the_p3_boundary_with_hue_preserved() {
    for (plan, levels) in [
        (bt2020_to_p3_sdr(), vec![0.25, 0.5, 1.0]),
        (bt2020_to_p3_hdr(), vec![0.25, 1.0, 4.0]),
    ] {
        let peak = plan.target_peak();

        for level in levels {
            for source in primaries_and_secondaries(level) {
                let mapped = reference::map(&plan, source);
                let source_lab = reference::source_lab(&plan, source);
                let mapped_lab = reference::target_lab(&plan, mapped);
                let (_, source_hue) = polar(source_lab);
                let (_, mapped_hue) = polar(mapped_lab);

                let lowest = mapped.iter().copied().fold(f64::INFINITY, f64::min);
                let highest = mapped.iter().copied().fold(0.0, f64::max);
                assert!(lowest >= 0.0 && highest <= peak, "{source:?}: {mapped:?}");
                assert!(
                    lowest < 1e-3 * peak || highest > (1.0 - 1e-3) * peak,
                    "{source:?} mapped inside the boundary: {mapped:?}"
                );
                assert!(
                    hue_difference_degrees(source_hue, mapped_hue) < HUE_TOLERANCE_DEGREES,
                    "{source:?}: hue {source_hue} -> {mapped_hue}"
                );
                assert!((source_lab[0] - mapped_lab[0]).abs() < 1e-3);
                if plan.space() == WorkingSpace::Ictcp {
                    assert!(hue_delta_e_itp(source_lab, mapped_lab) < MAX_HUE_DELTA_E_ITP);
                }
            }
        }
    }
}

#[test]
fn chroma_compression_is_monotonic() {
    for plan in [bt2020_to_p3_sdr(), bt2020_to_p3_hdr()] {
        let source_to_rgb = plan.work_to_source;

        for lightness_step in 1..6 {
            for hue_step in 0..24 {
                let angle = f64::from(hue_step) * std::f64::consts::TAU / 24.0;
                let hue = [angle.cos(), angle.sin()];
                let lightness = if plan.space() == WorkingSpace::Oklab {
                    f64::from(lightness_step) / 6.0
                } else {
                    reference::source_lab(&plan, [f64::from(lightness_step) / 3.0; 3])[0]
                };
                let source_limit =
                    reference::chroma_boundary(&plan, lightness, hue, &source_to_rgb, 1.0);
                let target_limit =
                    reference::chroma_boundary(&plan, lightness, hue, &plan.work_to_target, 1.0);

                let mut previous = 0.0;
                for step in 0..=100 {
                    let chroma = source_limit * f64::from(step) / 100.0;
                    let mapped =
                        reference::compress_chroma(chroma, target_limit, source_limit, plan.knee);

                    assert!(
                        mapped + 1e-12 >= previous,
                        "{chroma}: {mapped} < {previous}"
                    );
                    assert!(mapped <= target_limit + 1e-12);
                    if chroma <= plan.knee * target_limit {
                        assert!((mapped - chroma).abs() < 1e-12);
                    }
                    previous = mapped;
                }
                if source_limit > target_limit * 1.001 {
                    assert!((previous - target_limit).abs() < 1e-9);
                }
            }
        }
    }
}

#[test]
fn desaturate_keeps_luminance_when_possible() {
    let plan = ColorPlan::new(&settings(
        sdr(ColorPrimaries::Bt2020),
        sdr(ColorPrimaries::Bt709),
        GamutMapMode::Desaturate,
    ))
    .unwrap();

    let source = [0.0, 0.6, 0.0];
    let direct = plan.source_to_target().apply(source);
    let mapped = reference::map(&plan, source);
    let luminance = |rgb: [f64; 3]| {
        plan.target_luma
            .iter()
            .zip(rgb)
            .map(|(weight, value)| weight * value)
            .sum::<f64>()
    };

    assert!(mapped.iter().all(|value| (0.0..=1.0).contains(value)));
    assert!((luminance(mapped) - luminance(direct)).abs() < 1e-9);
}

#[test]
fn highlight_marks_only_out_of_gamut_colours() {
    let plan = ColorPlan::new(&settings(
        sdr(ColorPrimaries::Bt2020),
        sdr(ColorPrimaries::Bt709),
        GamutMapMode::Highlight,
    ))
    .unwrap();

    let grey = reference::map(&plan, [0.3; 3]);
    let green = reference::map(&plan, [0.0, 0.6, 0.0]);
    let clipped_green = plan
        .source_to_target()
        .apply([0.0, 0.6, 0.0])
        .map(|value| value.clamp(0.0, 1.0));

    assert!(max_difference(grey, [0.3; 3]) < 1e-9);
    assert!(max_difference(green, clipped_green.map(|value| 1.0 - value)) < 1e-9);
}

#[test]
fn invalid_settings_are_rejected() {
    let base = settings(
        sdr(ColorPrimaries::Bt2020),
        sdr(ColorPrimaries::Bt709),
        GamutMapMode::Perceptual,
    );
    let knee = ColorMapSettings {
        gamut: GamutMapping {
            knee: 1.0,
            ..base.gamut
        },
        ..base
    };
    let white = ColorMapSettings {
        target: ColorVolume {
            reference_white_nits: 0.0,
            ..base.target
        },
        ..base
    };
    let peak = ColorMapSettings {
        source: ColorVolume {
            peak_nits: 20_000.0,
            ..base.source
        },
        ..base
    };
    let black = ColorMapSettings {
        target: ColorVolume {
            black_nits: 500.0,
            ..base.target
        },
        ..base
    };

    assert_eq!(
        ColorPlan::new(&knee),
        Err(ScaleError::GamutKneeOutOfRange(1.0))
    );
    assert_eq!(
        ColorPlan::new(&white),
        Err(ScaleError::ReferenceWhiteNitsOutOfRange(0.0))
    );
    assert_eq!(
        ColorPlan::new(&peak),
        Err(ScaleError::PeakNitsOutOfRange(20_000.0))
    );
    assert!(matches!(
        ColorPlan::new(&black),
        Err(ScaleError::BlackNitsOutOfRange { .. })
    ));
}

fn test_image(width: u32, height: u32, scale: f32) -> Image {
    Image::from_fn(width, height, |x, y| {
        let index = y * width + x;
        #[allow(clippy::cast_possible_truncation)]
        let channel = |seed: u32| (unit_hash(index, seed) as f32).powi(2) * scale;
        match index % 4 {
            0 => [channel(0), 0.0, 0.0, 1.0],
            1 => [0.02 * scale, channel(1), 0.0, 0.5],
            _ => [channel(2), channel(3), channel(4), 1.0],
        }
    })
}

pub(super) fn map_on_gpu(gpu: &testing::Gpu, mapper: &mut ColorMapper, image: &Image) -> Image {
    let source = testing::upload(gpu, image);
    let target = testing::storage_target(gpu, image.width, image.height);

    testing::submit(gpu, |encoder| {
        mapper.encode(
            &gpu.device,
            encoder,
            &testing::view(&source),
            &testing::view(&target),
        );
    });
    testing::download(gpu, &target)
}

fn assert_gpu_matches_reference(settings: ColorMapSettings, scale: f32) {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut mapper = ColorMapper::new(&pipelines, &gpu.device, settings).unwrap();
    let image = test_image(37, 23, scale);

    let output_image = map_on_gpu(gpu, &mut mapper, &image);

    for (input, output) in image.pixels.iter().zip(&output_image.pixels) {
        let rgb = [0, 1, 2].map(|channel| f64::from(input[channel]));
        let want = reference::map(mapper.plan(), rgb);

        for channel in 0..3 {
            let got = f64::from(output[channel]);
            let tolerance = f64::from(RGBA16F_TOLERANCE) * want[channel].abs().max(1.0);
            assert!(
                (got - want[channel]).abs() <= tolerance,
                "{:?} {input:?}: {:?} != {want:?}",
                settings.gamut.mode,
                &output[..3]
            );
        }
        assert!((output[3] - input[3]).abs() < f32::EPSILON);
    }
}

#[test]
fn gpu_matches_reference_for_every_mode() {
    for mode in GamutMapMode::ALL {
        assert_gpu_matches_reference(
            settings(
                sdr(ColorPrimaries::Bt2020),
                sdr(ColorPrimaries::DisplayP3),
                mode,
            ),
            1.0,
        );
        assert_gpu_matches_reference(
            settings(
                hdr(ColorPrimaries::Bt2020, 1000.0),
                hdr(ColorPrimaries::DisplayP3, 1000.0),
                mode,
            ),
            4.5,
        );
    }
}

#[test]
fn gpu_matches_reference_for_a_contained_source() {
    assert_gpu_matches_reference(
        settings(
            sdr(ColorPrimaries::Bt709),
            sdr(ColorPrimaries::Bt2020),
            GamutMapMode::Perceptual,
        ),
        1.0,
    );
}

#[test]
fn reconfiguring_reports_what_changed() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let base = settings(
        sdr(ColorPrimaries::Bt2020),
        sdr(ColorPrimaries::Bt709),
        GamutMapMode::Perceptual,
    );
    let mut mapper = ColorMapper::new(&pipelines, &gpu.device, base).unwrap();

    let unchanged = mapper.configure(&gpu.device, base).unwrap();
    let changed = mapper
        .configure(
            &gpu.device,
            settings(base.source, base.target, GamutMapMode::Relative),
        )
        .unwrap();

    assert_eq!(unchanged, Reconfigured::Unchanged);
    assert_eq!(changed, Reconfigured::Parameters);
    assert_eq!(mapper.plan().path(), MappingPath::Clip);
}
