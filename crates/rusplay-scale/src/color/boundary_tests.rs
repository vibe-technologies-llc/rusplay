use rusplay_core::{ColorPrimaries, GamutMapMode, pq_to_nits};

use super::{
    tests::{hdr, map_on_gpu, max_difference, polar, sdr, settings, unit_hash},
    *,
};
use crate::{
    color::reference,
    testing::{self, Image, RGBA16F_TOLERANCE},
};

const GRAZING_BLUE: [f64; 3] = [0.281_25, 0.233_38, 0.546_875];
const GRAZING_GREEN_STEP: f64 = 0.0005;
const GRADIENT_STEPS: u32 = 2048;
const REFINEMENT: u32 = 256;
const SHRINK: f64 = 8.0;
const MAX_NEIGHBOUR_STEP: f64 = 0.02;
const MAX_CHROMA_STEP: f64 = 0.004;
const CHROMA_STEPS: u32 = 400;
const FOLD_HUES: std::ops::Range<u32> = 320..336;
const FOLD_HUE_SUBDIVISION: u32 = 8;
const TABLE_SAMPLES: u32 = 4096;
const MAX_TABLE_P99: f64 = 0.002;
const MAX_TABLE_ERROR: f64 = 0.06;

fn pq_4000_to_srgb() -> ColorPlan {
    ColorPlan::new(&settings(
        hdr(ColorPrimaries::Bt2020, 4000.0),
        ColorVolume::srgb_display(),
        GamutMapMode::Perceptual,
    ))
    .unwrap()
}

fn folding_plans() -> Vec<ColorPlan> {
    [
        settings(
            hdr(ColorPrimaries::Bt2020, 4000.0),
            ColorVolume::srgb_display(),
            GamutMapMode::Perceptual,
        ),
        settings(
            hdr(ColorPrimaries::Bt2020, 1000.0),
            hdr(ColorPrimaries::Bt709, 1000.0),
            GamutMapMode::Perceptual,
        ),
        settings(
            hdr(ColorPrimaries::Bt2020, 1000.0),
            hdr(ColorPrimaries::DisplayP3, 1000.0),
            GamutMapMode::Perceptual,
        ),
        settings(
            sdr(ColorPrimaries::Bt2020),
            sdr(ColorPrimaries::Bt709),
            GamutMapMode::Perceptual,
        ),
    ]
    .iter()
    .map(|settings| ColorPlan::new(settings).unwrap())
    .collect()
}

fn linear(plan: &ColorPlan, code: [f64; 3]) -> [f64; 3] {
    if plan.space() == WorkingSpace::Ictcp {
        code.map(|channel| pq_to_nits(channel) / plan.source_white_nits)
    } else {
        code.map(|channel| channel.powf(2.4))
    }
}

fn gradients() -> Vec<([f64; 3], [f64; 3])> {
    let mut lines = vec![(
        [GRAZING_BLUE[0], 0.2, GRAZING_BLUE[2]],
        [GRAZING_BLUE[0], 0.27, GRAZING_BLUE[2]],
    )];
    for level in [0.35, 0.45, 0.55, 0.65, 0.75] {
        lines.push(([0.0, 0.0, level], [level, 0.0, level]));
        lines.push((
            [0.2 * level, 0.1 * level, level],
            [level, 0.1 * level, level],
        ));
        lines.push(([0.0, 0.0, level], [0.0, level, level]));
        lines.push(([0.5 * level, 0.0, level], [0.5 * level, 0.6 * level, level]));
    }
    lines
}

fn along(line: ([f64; 3], [f64; 3]), step: u32) -> [f64; 3] {
    let fraction = f64::from(step) / f64::from(GRADIENT_STEPS);

    [0, 1, 2].map(|channel| line.0[channel] + fraction * (line.1[channel] - line.0[channel]))
}

fn largest_step(outputs: &[[f64; 3]]) -> (f64, usize) {
    outputs
        .windows(2)
        .enumerate()
        .map(|(index, pair)| (max_difference(pair[0], pair[1]), index))
        .fold(
            (0.0, 0),
            |worst, step| if step.0 > worst.0 { step } else { worst },
        )
}

#[test]
fn neighbouring_codes_on_a_grazing_ray_map_alike() {
    let plan = pq_4000_to_srgb();
    let neighbour = [
        GRAZING_BLUE[0],
        GRAZING_BLUE[1] + GRAZING_GREEN_STEP,
        GRAZING_BLUE[2],
    ];

    let first = reference::map(&plan, linear(&plan, GRAZING_BLUE));
    let second = reference::map(&plan, linear(&plan, neighbour));

    assert!(
        max_difference(first, second) < MAX_NEIGHBOUR_STEP,
        "{first:?} vs {second:?}"
    );
}

fn worst_interval(map: impl Fn([f64; 3]) -> [f64; 3], line: ([f64; 3], [f64; 3])) -> Interval {
    let outputs: Vec<[f64; 3]> = (0..=GRADIENT_STEPS)
        .map(|step| map(along(line, step)))
        .collect();
    let (step, index) = largest_step(&outputs);
    let index = u32::try_from(index).unwrap();

    Interval {
        from: along(line, index),
        to: along(line, index + 1),
        step,
    }
}

struct Interval {
    from: [f64; 3],
    to: [f64; 3],
    step: f64,
}

#[test]
fn perceptual_mapping_is_continuous_along_saturated_gradients() {
    for plan in folding_plans() {
        let map = |code| reference::map(&plan, linear(&plan, code));

        for line in gradients() {
            let worst = worst_interval(map, line);
            let refined: Vec<[f64; 3]> = (0..=REFINEMENT)
                .map(|step| map(along_interval(&worst, step)))
                .collect();

            let (step, _) = largest_step(&refined);
            assert!(
                step <= worst.step / SHRINK,
                "{:?} {line:?}: a step of {} near {:?} only refines to {step}",
                plan.space(),
                worst.step,
                worst.from
            );
        }
    }
}

fn along_interval(interval: &Interval, step: u32) -> [f64; 3] {
    let fraction = f64::from(step) / f64::from(REFINEMENT);

    [0, 1, 2].map(|channel| {
        interval.from[channel] + fraction * (interval.to[channel] - interval.from[channel])
    })
}

#[test]
fn mapped_chroma_grows_steadily_through_the_fold() {
    let plan = ColorPlan::new(&settings(
        hdr(ColorPrimaries::Bt2020, 1000.0),
        hdr(ColorPrimaries::Bt709, 1000.0),
        GamutMapMode::Perceptual,
    ))
    .unwrap();
    let to_source = plan.work_to_source;

    assert!(plan.tone().is_none());
    for lightness_step in 1..8 {
        let lightness = reference::source_lab(&plan, [f64::from(lightness_step) / 4.0; 3])[0];
        for hue_step in FOLD_HUES.start * FOLD_HUE_SUBDIVISION..FOLD_HUES.end * FOLD_HUE_SUBDIVISION
        {
            let angle = (f64::from(hue_step) / f64::from(FOLD_HUE_SUBDIVISION)).to_radians();
            let hue = [angle.cos(), angle.sin()];
            let source_limit = reference::chroma_boundary(
                &plan,
                lightness,
                hue,
                &to_source,
                plan.source_chroma_limit,
            );

            let mut previous = 0.0;
            for step in 0..=CHROMA_STEPS {
                let chroma = source_limit * f64::from(step) / f64::from(CHROMA_STEPS);
                let work =
                    reference::lab_to_xyz(&plan, [lightness, hue[0] * chroma, hue[1] * chroma]);
                let mapped = reference::map(&plan, to_source.apply(work).map(|v| v / plan.gain));
                let (mapped_chroma, _) = polar(reference::target_lab(&plan, mapped));

                assert!(
                    mapped_chroma + 1e-6 >= previous,
                    "{lightness} {angle}: {mapped_chroma} < {previous} at {chroma}"
                );
                assert!(
                    mapped_chroma - previous < MAX_CHROMA_STEP,
                    "{lightness} {angle}: {previous} -> {mapped_chroma} at {chroma}"
                );
                previous = mapped_chroma;
            }
        }
    }
}

#[test]
fn gpu_mapping_follows_the_reference_along_saturated_gradients() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let lines = gradients();
    let width = GRADIENT_STEPS + 1;

    for settings in [
        settings(
            hdr(ColorPrimaries::Bt2020, 4000.0),
            ColorVolume::srgb_display(),
            GamutMapMode::Perceptual,
        ),
        settings(
            hdr(ColorPrimaries::Bt2020, 1000.0),
            hdr(ColorPrimaries::Bt709, 1000.0),
            GamutMapMode::Perceptual,
        ),
        settings(
            sdr(ColorPrimaries::Bt2020),
            sdr(ColorPrimaries::Bt709),
            GamutMapMode::Perceptual,
        ),
    ] {
        let mut mapper = ColorMapper::new(&pipelines, &gpu.device, settings).unwrap();
        let plan = mapper.plan().clone();
        let quantum = f64::from(RGBA16F_TOLERANCE) * plan.target_peak().max(1.0);
        let image = Image::from_fn(width, u32::try_from(lines.len()).unwrap(), |x, y| {
            #[allow(clippy::cast_possible_truncation)]
            let rgb = linear(&plan, along(lines[y as usize], x)).map(|v| v as f32);
            [rgb[0], rgb[1], rgb[2], 1.0]
        });

        let texels = map_on_gpu(gpu, &mut mapper, &image);

        for (y, line) in lines.iter().enumerate() {
            let row = y * width as usize..(y + 1) * width as usize;
            let found: Vec<[f64; 3]> = texels.pixels[row.clone()]
                .iter()
                .map(|texel| [0, 1, 2].map(|channel| f64::from(texel[channel])))
                .collect();
            let expected: Vec<[f64; 3]> = image.pixels[row]
                .iter()
                .map(|texel| {
                    let rgb =
                        [0, 1, 2].map(|channel| f64::from(testing::quantize_f16(texel[channel])));
                    reference::map(&plan, rgb)
                })
                .collect();
            let worst = found
                .iter()
                .zip(&expected)
                .map(|(found, expected)| max_difference(*found, *expected))
                .fold(0.0, f64::max);

            assert!(
                worst <= 3.0 * quantum,
                "{:?} {line:?}: gpu differs by {worst}",
                plan.space()
            );
            assert!(
                largest_step(&found).0 <= largest_step(&expected).0 + 2.0 * quantum,
                "{:?} {line:?}: gpu steps {} against {}",
                plan.space(),
                largest_step(&found).0,
                largest_step(&expected).0
            );
        }
    }
}

#[test]
fn gpu_maps_neighbouring_grazing_codes_alike() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut mapper = ColorMapper::new(
        &pipelines,
        &gpu.device,
        settings(
            hdr(ColorPrimaries::Bt2020, 4000.0),
            ColorVolume::srgb_display(),
            GamutMapMode::Perceptual,
        ),
    )
    .unwrap();
    let plan = mapper.plan().clone();
    let codes = [
        GRAZING_BLUE,
        [
            GRAZING_BLUE[0],
            GRAZING_BLUE[1] + GRAZING_GREEN_STEP,
            GRAZING_BLUE[2],
        ],
    ];
    let image = Image::from_fn(2, 1, |x, _| {
        #[allow(clippy::cast_possible_truncation)]
        let rgb = linear(&plan, codes[x as usize]).map(|channel| channel as f32);
        [rgb[0], rgb[1], rgb[2], 1.0]
    });

    let texels = map_on_gpu(gpu, &mut mapper, &image);
    let outputs: Vec<[f64; 3]> = texels
        .pixels
        .iter()
        .map(|texel| [0, 1, 2].map(|channel| f64::from(texel[channel])))
        .collect();
    let expected = reference::map(&plan, linear(&plan, GRAZING_BLUE));

    assert!(
        max_difference(outputs[0], outputs[1]) < MAX_NEIGHBOUR_STEP,
        "{outputs:?}"
    );
    assert!(
        max_difference(outputs[0], expected) < MAX_NEIGHBOUR_STEP,
        "{outputs:?} vs {expected:?}"
    );
}

#[test]
fn the_boundary_table_tracks_the_exact_boundary() {
    for plan in folding_plans() {
        let mut errors: Vec<f64> = (0..TABLE_SAMPLES)
            .map(|index| {
                let lightness = plan.boundary_lightness * unit_hash(index, 0);
                let angle = std::f64::consts::TAU * unit_hash(index, 1);
                let [table, _] = reference::boundary_limits(&plan, lightness, angle);
                let exact = reference::chroma_boundary(
                    &plan,
                    lightness,
                    [angle.cos(), angle.sin()],
                    &plan.work_to_target,
                    plan.target_chroma_limit,
                );
                (table - exact).abs() / plan.target_chroma_limit
            })
            .collect();
        errors.sort_by(f64::total_cmp);

        let p99 = errors[errors.len() * 99 / 100];
        let worst = errors[errors.len() - 1];
        assert!(
            p99 <= MAX_TABLE_P99,
            "{:?}: p99 {p99} of the chroma limit",
            plan.space()
        );
        assert!(
            worst <= MAX_TABLE_ERROR,
            "{:?}: {worst} of the chroma limit",
            plan.space()
        );
    }
}
