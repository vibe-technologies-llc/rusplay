use rusplay_core::{ColorPrimaries, GamutMapMode, ToneMapOperator, pq_from_nits};

use super::{
    tests::{
        hdr, hue_difference_degrees, map_on_gpu, polar, primaries_and_secondaries, sdr, unit_hash,
    },
    *,
};
use crate::{
    color::reference::{self, PeakState},
    testing::{self, Image, RGBA16F_TOLERANCE},
};

const PQ_1000_NITS: f64 = 0.751_827_096;
const PQ_4000_NITS: f64 = 0.902_572_393;
const BT2390_4000_TO_1000_MAX_LUM: f64 = 0.832_982_597;
const BT2390_4000_TO_1000_KNEE: f64 = 0.749_473_896;
const HUE_TOLERANCE_DEGREES: f64 = 2.0;

fn curve(operator: ToneMapOperator, source_peak: f64, target_peak: f64) -> ToneCurve {
    ToneCurve {
        operator,
        source_peak_nits: source_peak,
        source_black_nits: 0.0,
        target_peak_nits: target_peak,
        target_black_nits: 0.0,
    }
}

fn tone_settings(
    source: ColorVolume,
    target: ColorVolume,
    operator: ToneMapOperator,
    dynamic_peak: Option<DynamicPeak>,
) -> ColorMapSettings {
    ColorMapSettings {
        source,
        target,
        tone: ToneMapping {
            operator,
            dynamic_peak,
        },
        gamut: GamutMapping::default(),
    }
}

#[test]
fn bt2390_matches_its_reference_points() {
    let eetf = curve(ToneMapOperator::Bt2390, 4000.0, 1000.0);

    assert!((pq_from_nits(1000.0) - PQ_1000_NITS).abs() < 1e-8);
    assert!((pq_from_nits(4000.0) - PQ_4000_NITS).abs() < 1e-8);
    assert!((eetf.max_lum() - BT2390_4000_TO_1000_MAX_LUM).abs() < 1e-6);
    assert!((eetf.knee_start() - BT2390_4000_TO_1000_KNEE).abs() < 1e-6);
    assert!(eetf.min_lum().abs() < f64::EPSILON);

    assert!((eetf.bt2390(0.0)).abs() < 1e-12);
    assert!((eetf.bt2390(0.5) - 0.5).abs() < 1e-12);
    assert!((eetf.bt2390(eetf.knee_start()) - eetf.knee_start()).abs() < 1e-12);
    assert!((eetf.bt2390(1.0) - eetf.max_lum()).abs() < 1e-12);
    assert!((eetf.map_nits(4000.0) - 1000.0).abs() < 1e-6);
    assert!((eetf.map_nits(100.0) - 100.0).abs() < 1e-9);
}

#[test]
fn bt2390_is_smooth_at_the_knee_and_monotonic() {
    let eetf = curve(ToneMapOperator::Bt2390, 4000.0, 1000.0);
    let knee = eetf.knee_start();
    let step = 1e-6;

    let below = (eetf.bt2390(knee) - eetf.bt2390(knee - step)) / step;
    let above = (eetf.bt2390(knee + step) - eetf.bt2390(knee)) / step;

    assert!((below - 1.0).abs() < 1e-4);
    assert!((above - 1.0).abs() < 1e-4);
    let mut previous = 0.0;
    for index in 0..=1000 {
        let mapped = eetf.bt2390(f64::from(index) / 1000.0);
        assert!(mapped >= previous && mapped <= eetf.max_lum() + 1e-12);
        previous = mapped;
    }
}

#[test]
fn bt2390_lifts_black_to_the_target_black() {
    let eetf = ToneCurve {
        target_black_nits: 0.1,
        ..curve(ToneMapOperator::Bt2390, 1000.0, 400.0)
    };

    assert!((eetf.map_nits(0.0) - 0.1).abs() < 1e-9);
    assert!((eetf.bt2390(0.0) - eetf.min_lum()).abs() < 1e-12);
    assert!((eetf.map_nits(1000.0) / 400.0 - 1.0).abs() < 1e-3);
}

#[test]
fn every_operator_is_monotonic_and_lands_on_the_target_peak() {
    for operator in ToneMapOperator::ALL {
        for (source_peak, target_peak) in [(4000.0, 1000.0), (1000.0, 203.0), (10_000.0, 100.0)] {
            let tone = curve(operator, source_peak, target_peak);

            let mut previous = 0.0;
            for index in 0..=2000 {
                let nits = source_peak * (f64::from(index) / 2000.0).powi(3);
                let mapped = tone.map_nits(nits);
                assert!(
                    mapped + 1e-9 >= previous,
                    "{operator} {source_peak}: {nits} -> {mapped} < {previous}"
                );
                assert!(mapped <= target_peak * (1.0 + 1e-9));
                previous = mapped;
            }
            assert!(
                (tone.map_nits(source_peak) - target_peak).abs() < 1e-6 * target_peak,
                "{operator} {source_peak} -> {}",
                tone.map_nits(source_peak)
            );
        }
    }
}

#[test]
fn shadows_pass_through_the_pq_operators() {
    for (operator, highest) in [
        (ToneMapOperator::Bt2390, 300.0),
        (ToneMapOperator::Spline, 100.0),
        (ToneMapOperator::Clip, 1000.0),
    ] {
        let tone = curve(operator, 4000.0, 1000.0);

        for nits in [0.01, 1.0, 10.0, highest] {
            assert!(
                (tone.map_nits(nits) - nits).abs() < 1e-9 * nits.max(1.0),
                "{operator}"
            );
        }
    }
}

#[test]
fn plan_tone_maps_only_when_the_peak_exceeds_the_target() {
    let plan = |source, target, dynamic| {
        ColorPlan::new(&tone_settings(
            source,
            target,
            ToneMapOperator::Bt2390,
            dynamic,
        ))
        .unwrap()
        .tone()
    };

    let to_sdr = plan(
        hdr(ColorPrimaries::Bt2020, 1000.0),
        sdr(ColorPrimaries::DisplayP3),
        None,
    )
    .unwrap();

    assert!((to_sdr.curve.target_peak_nits - 203.0).abs() < f64::EPSILON);
    assert!((to_sdr.curve.source_peak_nits - 1000.0).abs() < f64::EPSILON);
    assert_eq!(
        plan(
            hdr(ColorPrimaries::Bt2020, 1000.0),
            hdr(ColorPrimaries::DisplayP3, 1000.0),
            None
        ),
        None
    );
    assert_eq!(
        plan(sdr(ColorPrimaries::Bt709), sdr(ColorPrimaries::Bt709), None),
        None
    );
    assert!(
        plan(
            hdr(ColorPrimaries::Bt2020, 600.0),
            hdr(ColorPrimaries::DisplayP3, 1000.0),
            Some(DynamicPeak::default())
        )
        .is_some()
    );
}

#[test]
fn content_metadata_sets_the_source_peak() {
    let mut color = rusplay_core::ColorSpace {
        primaries: ColorPrimaries::Bt2020,
        transfer: rusplay_core::TransferFunction::Pq,
        ..rusplay_core::ColorSpace::default()
    };
    color.hdr.content_light = Some(rusplay_core::ContentLightLevel {
        max_cll_nits: 2500,
        max_fall_nits: 400,
    });

    let volume = ColorVolume::of_content(&color);

    assert!((volume.peak_nits - 2500.0).abs() < f64::EPSILON);
}

#[test]
fn tone_mapping_holds_hue_and_scales_chroma_with_intensity() {
    let plan = ColorPlan::new(&tone_settings(
        hdr(ColorPrimaries::Bt2020, 4000.0),
        hdr(ColorPrimaries::Bt2020, 600.0),
        ToneMapOperator::Bt2390,
        None,
    ))
    .unwrap();
    let curve = plan.tone().unwrap().curve;

    for index in 0..200 {
        let rgb = [0, 1, 2].map(|channel| 19.0 * unit_hash(index, channel).powi(2));
        let before = reference::tone_intensity(&plan, rgb, plan.source_white_nits);
        let after_rgb = reference::tone_map(&plan, &curve, rgb);
        let after = reference::tone_intensity(&plan, after_rgb, plan.target_white_nits);

        let (chroma_before, hue_before) = polar(before);
        let (chroma_after, hue_after) = polar(after);
        let ratio = (after[0] / before[0]).min(before[0] / after[0]);

        assert!((after[0] - curve.map_pq(before[0])).abs() < 1e-9);
        if chroma_before > 1e-6 {
            assert!(hue_difference_degrees(hue_before, hue_after) < 1e-6);
            assert!((chroma_after - chroma_before * ratio).abs() < 1e-9);
        }
    }
}

#[test]
fn hdr_bt2020_reaches_an_sdr_p3_display_in_range_with_hue_held() {
    let plan = ColorPlan::new(&tone_settings(
        hdr(ColorPrimaries::Bt2020, 1000.0),
        sdr(ColorPrimaries::DisplayP3),
        ToneMapOperator::Bt2390,
        None,
    ))
    .unwrap();

    assert_eq!(plan.path(), MappingPath::Perceptual);
    for level in [0.5, 1.0, 2.0, 4.9] {
        for source in primaries_and_secondaries(level) {
            let mapped = reference::map(&plan, source);
            let (_, source_hue) = polar(reference::source_lab(&plan, source));
            let (_, mapped_hue) = polar(reference::target_lab(&plan, mapped));

            assert!(
                mapped.iter().all(|value| (0.0..=1.0).contains(value)),
                "{mapped:?}"
            );
            assert!(
                hue_difference_degrees(source_hue, mapped_hue) < HUE_TOLERANCE_DEGREES,
                "{source:?}: {source_hue} -> {mapped_hue}"
            );
        }
    }

    let mut previous = 0.0;
    for step in 0..=50 {
        let grey = reference::map(&plan, [f64::from(step) / 10.0; 3]);
        assert!(grey[1] >= previous - 1e-9 && grey[1] <= 1.0);
        previous = grey[1];
    }
}

#[test]
fn invalid_dynamic_settings_are_rejected() {
    let with = |dynamic| {
        ColorPlan::new(&tone_settings(
            hdr(ColorPrimaries::Bt2020, 1000.0),
            sdr(ColorPrimaries::Bt709),
            ToneMapOperator::Bt2390,
            Some(dynamic),
        ))
    };

    assert_eq!(
        with(DynamicPeak {
            percentile: 0.0,
            ..DynamicPeak::default()
        }),
        Err(ScaleError::PeakPercentileOutOfRange(0.0))
    );
    assert_eq!(
        with(DynamicPeak {
            smoothing_frames: 0,
            ..DynamicPeak::default()
        }),
        Err(ScaleError::ZeroSmoothingFrames)
    );
    assert_eq!(
        with(DynamicPeak {
            scene_threshold_db: -1.0,
            ..DynamicPeak::default()
        }),
        Err(ScaleError::SceneThresholdOutOfRange(-1.0))
    );
}

fn hdr_image(width: u32, height: u32, scale: f32, seed: u32) -> Image {
    Image::from_fn(width, height, |x, y| {
        let index = y * width + x;
        #[allow(clippy::cast_possible_truncation)]
        let channel = |offset: u32| (unit_hash(index, seed + offset) as f32).powi(3) * scale;
        [channel(0), channel(1), channel(2), 1.0]
    })
}

fn assert_matches(plan: &ColorPlan, curve: Option<ToneCurve>, image: &Image, output: &Image) {
    for (input, got) in image.pixels.iter().zip(&output.pixels) {
        let rgb = [0, 1, 2].map(|channel| f64::from(input[channel]));
        let want = reference::map_with_curve(plan, curve, rgb);

        for channel in 0..3 {
            let tolerance = 2.0 * f64::from(RGBA16F_TOLERANCE) * want[channel].abs().max(1.0);
            assert!(
                (f64::from(got[channel]) - want[channel]).abs() <= tolerance,
                "{input:?}: {:?} != {want:?}",
                &got[..3]
            );
        }
    }
}

#[test]
fn gpu_matches_reference_for_every_operator() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let image = hdr_image(41, 19, 19.0, 3);

    for operator in ToneMapOperator::ALL {
        for (target, mode) in [
            (sdr(ColorPrimaries::DisplayP3), GamutMapMode::Perceptual),
            (
                hdr(ColorPrimaries::DisplayP3, 600.0),
                GamutMapMode::Perceptual,
            ),
            (hdr(ColorPrimaries::Bt2020, 600.0), GamutMapMode::Relative),
        ] {
            let settings = ColorMapSettings {
                gamut: GamutMapping {
                    mode,
                    ..GamutMapping::default()
                },
                ..tone_settings(hdr(ColorPrimaries::Bt2020, 4000.0), target, operator, None)
            };
            let mut mapper = ColorMapper::new(&pipelines, &gpu.device, settings).unwrap();

            let output = map_on_gpu(gpu, &mut mapper, &image);

            let plan = mapper.plan();
            assert_matches(plan, plan.tone().map(|stage| stage.curve), &image, &output);
        }
    }
}

fn read_state(gpu: &testing::Gpu, mapper: &ColorMapper) -> (PeakState, f64) {
    let bytes = testing::download_buffer(gpu, mapper.tone_state());
    let word = |index: usize| {
        u32::from_ne_bytes([
            bytes[4 * index],
            bytes[4 * index + 1],
            bytes[4 * index + 2],
            bytes[4 * index + 3],
        ])
    };
    let float = |index: usize| f64::from(f32::from_bits(word(index)));

    (
        PeakState {
            smoothed_peak_pq: float(8),
            previous_average_pq: float(9),
            frames: word(10),
        },
        float(0),
    )
}

#[test]
fn dynamic_peak_follows_frames_smooths_and_resets_on_scene_cuts() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let dynamic = DynamicPeak {
        percentile: 99.0,
        smoothing_frames: 4,
        scene_threshold_db: 3.0,
    };
    let settings = tone_settings(
        hdr(ColorPrimaries::Bt2020, 4000.0),
        hdr(ColorPrimaries::DisplayP3, 400.0),
        ToneMapOperator::Bt2390,
        Some(dynamic),
    );
    let mut mapper = ColorMapper::new(&pipelines, &gpu.device, settings).unwrap();
    let stage = mapper.plan().tone().unwrap();
    let frames = [
        hdr_image(64, 48, 10.0, 7),
        hdr_image(64, 48, 11.0, 9),
        hdr_image(64, 48, 0.5, 11),
    ];

    let mut expected = PeakState::default();
    for (index, image) in frames.iter().enumerate() {
        let output = map_on_gpu(gpu, &mut mapper, image);
        let bins = reference::histogram(mapper.plan(), &image.pixels);
        let (next, curve) = reference::detect_peak(&stage, expected, &bins);
        let (measured, source_peak) = read_state(gpu, &mapper);

        assert_eq!(measured.frames, next.frames);
        assert!(
            (measured.smoothed_peak_pq - next.smoothed_peak_pq).abs() < 1e-5,
            "frame {index}: {measured:?} != {next:?}"
        );
        assert!((measured.previous_average_pq - next.previous_average_pq).abs() < 1e-5);
        assert!((source_peak - curve.source_peak_nits).abs() < 1e-3 * curve.source_peak_nits);
        assert_matches(mapper.plan(), Some(curve), image, &output);

        let follows_frame = (next.smoothed_peak_pq - frame_peak(&bins, 0.99)).abs() < 1e-12;
        if index == 1 {
            assert!(next.smoothed_peak_pq < frame_peak(&bins, 0.99));
        } else {
            assert!(follows_frame, "frame {index} did not reset to its own peak");
        }
        expected = next;
    }
}

fn frame_peak(bins: &[u32], fraction: f64) -> f64 {
    let total: u32 = bins.iter().sum();
    let mut cumulative = 0;
    let bin = bins
        .iter()
        .position(|count| {
            cumulative += count;
            f64::from(cumulative) >= fraction * f64::from(total)
        })
        .unwrap();

    #[allow(clippy::cast_precision_loss)]
    let peak = (bin + 1) as f64 / f64::from(HISTOGRAM_BINS);
    peak
}
