use rusplay_core::{
    ColorPrimaries, ColorSpace, Matrix3, PixelFormat, TransferFunction, WhiteAdaptation,
    pq_from_nits, pq_to_nits,
};
use rusplay_scale::{ColorVolume, ScalePipelines};

use super::*;
use crate::{Gpu, testing};

const WHITE: f64 = 203.0;
const GAMUT_TOLERANCE: f64 = 2e-3;

const OKLAB_LMS: [[f64; 3]; 3] = [
    [0.818_933_010_1, 0.361_866_742_4, -0.128_859_713_7],
    [0.032_984_543_6, 0.929_311_871_5, 0.036_145_638_7],
    [0.048_200_301_8, 0.264_366_269_1, 0.633_851_707_0],
];
const OKLAB_LAB: [[f64; 3]; 3] = [
    [0.210_454_255_3, 0.793_617_785_0, -0.004_072_046_8],
    [1.977_998_495_1, -2.428_592_205_0, 0.450_593_709_9],
    [0.025_904_037_1, 0.782_771_766_2, -0.808_675_766_0],
];
const ICTCP_LMS: [[f64; 3]; 3] = [
    [1688.0 / 4096.0, 2146.0 / 4096.0, 262.0 / 4096.0],
    [683.0 / 4096.0, 2951.0 / 4096.0, 462.0 / 4096.0],
    [99.0 / 4096.0, 309.0 / 4096.0, 3688.0 / 4096.0],
];
const ICTCP_ITP: [[f64; 3]; 3] = [
    [0.5, 0.5, 0.0],
    [6610.0 / 4096.0, -13613.0 / 4096.0, 7003.0 / 4096.0],
    [17933.0 / 4096.0, -17390.0 / 4096.0, -543.0 / 4096.0],
];

fn apply(rows: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    rows.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

fn oklab(primaries: ColorPrimaries, rgb: [f64; 3]) -> [f64; 3] {
    let xyz = primaries.rgb_to_xyz().apply(rgb);
    let lms = apply(OKLAB_LMS, xyz).map(f64::cbrt);
    apply(OKLAB_LAB, lms)
}

fn ictcp(primaries: ColorPrimaries, rgb_nits: [f64; 3]) -> [f64; 3] {
    let bt2020 = primaries
        .rgb_to_rgb(ColorPrimaries::Bt2020, WhiteAdaptation::Bradford)
        .apply(rgb_nits);
    let lms = apply(ICTCP_LMS, bt2020).map(pq_from_nits);
    apply(ICTCP_ITP, lms)
}

fn hue_degrees(lab: [f64; 3]) -> f64 {
    lab[2].atan2(lab[1]).to_degrees()
}

fn hue_difference(a: [f64; 3], b: [f64; 3]) -> f64 {
    let difference = (hue_degrees(a) - hue_degrees(b)).rem_euclid(360.0);
    difference.min(360.0 - difference)
}

fn widen(texel: [f32; 4]) -> [f64; 3] {
    [texel[0], texel[1], texel[2]].map(f64::from)
}

fn narrow(rgb: [f64; 3]) -> [f32; 4] {
    #[allow(clippy::cast_possible_truncation)]
    let channels = rgb.map(|channel| testing::quantized(channel as f32));
    [channels[0], channels[1], channels[2], 1.0]
}

fn clip(rgb: [f64; 3]) -> [f64; 3] {
    rgb.map(|channel| channel.clamp(0.0, 1.0))
}

fn in_gamut(rgb: [f64; 3]) -> bool {
    rgb.iter()
        .all(|channel| (-GAMUT_TOLERANCE..=1.0 + GAMUT_TOLERANCE).contains(channel))
}

fn frame_color(primaries: ColorPrimaries, transfer: TransferFunction) -> FrameColor {
    FrameColor::of_stream(
        ColorSpace {
            primaries,
            transfer,
            ..ColorSpace::default()
        },
        PixelFormat::default(),
    )
}

fn run(
    gpu: &Gpu,
    stage: &mut ColorStage,
    source: FrameColor,
    target: ColorVolume,
    texels: &[[f32; 4]],
) -> (ColorRoute, Vec<[f32; 4]>) {
    let width = u32::try_from(texels.len()).unwrap();
    let route = stage.route(&gpu.device, source, target).unwrap();
    let input = testing::upload(gpu, width, 1, texels);
    let linear = testing::storage(gpu, width, 1);
    let mapped = testing::storage(gpu, width, 1);
    let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());
    let linear_view = linear.create_view(&wgpu::TextureViewDescriptor::default());
    let mapped_view = mapped.create_view(&wgpu::TextureViewDescriptor::default());

    let mut current = &input;
    testing::submit(gpu, |encoder| {
        let mut view = &input_view;
        if route.linearise.is_some() {
            stage
                .linearise(&gpu.device, encoder, view, &linear_view)
                .unwrap();
            view = &linear_view;
            current = &linear;
        }
        if route.map.is_some() {
            stage.map(&gpu.device, encoder, view, &mapped_view);
            current = &mapped;
        }
    });

    (route, testing::read_rgba16f(gpu, current))
}

fn stage(gpu: &Gpu) -> ColorStage {
    ColorStage::new(
        &ScalePipelines::new(&gpu.device),
        MappingOptions::from(&ColorConfig::default()),
    )
}

#[test]
fn pq_greys_follow_the_bt2390_curve_onto_an_sdr_display() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let mut stage = stage(&gpu);
    let source = frame_color(ColorPrimaries::Bt2020, TransferFunction::Pq);
    let nits = [0.5, 5.0, 40.0, 100.0, 203.0, 400.0, 700.0, 1000.0];
    let texels: Vec<[f32; 4]> = nits
        .iter()
        .map(|nits| narrow([pq_from_nits(*nits); 3]))
        .collect();

    let (route, output) = run(
        &gpu,
        &mut stage,
        source,
        ColorVolume::srgb_display(),
        &texels,
    );
    let curve = stage
        .mapper()
        .and_then(|mapper| mapper.plan().tone())
        .unwrap()
        .curve;

    assert!(route.linearise.is_some());
    assert!(route.map.is_some());
    assert_eq!(route.output.light, OutputLight::Linear);
    assert_eq!(route.output.space.primaries, ColorPrimaries::Bt709);
    for (texel, found) in texels.iter().zip(&output) {
        let nits = pq_to_nits(f64::from(texel[0]));
        let expected = curve.map_nits(nits) / WHITE;
        for channel in widen(*found) {
            assert!(
                (channel - expected).abs() <= 2e-3 + expected * 5e-3,
                "{nits} cd/m²: {channel} against {expected}"
            );
        }
    }
    assert!((f64::from(output[7][0]) - 1.0).abs() < 5e-3);
}

#[test]
fn saturated_pq_colours_land_in_the_sdr_gamut_with_their_hue() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let mut stage = stage(&gpu);
    let source = frame_color(ColorPrimaries::Bt2020, TransferFunction::Pq);
    let colours: [[f64; 3]; 6] = [
        [600.0, 0.0, 0.0],
        [0.0, 600.0, 0.0],
        [0.0, 0.0, 600.0],
        [0.0, 500.0, 500.0],
        [800.0, 0.0, 800.0],
        [700.0, 700.0, 0.0],
    ];
    let texels: Vec<[f32; 4]> = colours
        .iter()
        .map(|colour| narrow(colour.map(pq_from_nits)))
        .collect();
    let to_srgb =
        ColorPrimaries::Bt2020.rgb_to_rgb(ColorPrimaries::Bt709, WhiteAdaptation::Bradford);

    let (_, output) = run(
        &gpu,
        &mut stage,
        source,
        ColorVolume::srgb_display(),
        &texels,
    );

    for (texel, found) in texels.iter().zip(&output) {
        let nits = widen(*texel).map(pq_to_nits);
        let mapped = widen(*found);
        let source_hue = ictcp(ColorPrimaries::Bt2020, nits);
        let mapped_hue = ictcp(ColorPrimaries::Bt709, mapped.map(|channel| channel * WHITE));
        let clipped = clip(to_srgb.apply(nits.map(|channel| channel / WHITE)));
        let clipped_hue = ictcp(
            ColorPrimaries::Bt709,
            clipped.map(|channel| channel * WHITE),
        );
        let held = hue_difference(source_hue, mapped_hue);

        assert!(in_gamut(mapped), "{nits:?} mapped to {mapped:?}");
        assert!(held < 3.0, "{nits:?} hue moved {held}°");
        assert!(held <= hue_difference(source_hue, clipped_hue) + 0.5);
    }
}

#[test]
fn bt2020_primaries_compress_perceptually_into_srgb_without_hue_shift() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let mut stage = stage(&gpu);
    let source = frame_color(ColorPrimaries::Bt2020, TransferFunction::Srgb);
    let to_srgb: Matrix3 =
        ColorPrimaries::Bt2020.rgb_to_rgb(ColorPrimaries::Bt709, WhiteAdaptation::Bradford);
    let inside = ColorPrimaries::Bt709
        .rgb_to_rgb(ColorPrimaries::Bt2020, WhiteAdaptation::Bradford)
        .apply([0.42, 0.36, 0.3]);
    let colours: [[f64; 3]; 8] = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.7, 0.7],
        [0.7, 0.0, 0.7],
        [0.8, 0.8, 0.0],
        [0.1, 0.6, 0.15],
        inside,
    ];
    let texels: Vec<[f32; 4]> = colours.iter().map(|colour| narrow(*colour)).collect();

    let (route, output) = run(
        &gpu,
        &mut stage,
        source,
        ColorVolume::srgb_display(),
        &texels,
    );

    assert!(route.linearise.is_none());
    assert!(route.map.is_some());
    for (texel, found) in texels.iter().zip(&output) {
        let colour = widen(*texel);
        let mapped = widen(*found);
        let source_lab = oklab(ColorPrimaries::Bt2020, colour);
        let mapped_lab = oklab(ColorPrimaries::Bt709, mapped);
        let clipped_lab = oklab(ColorPrimaries::Bt709, clip(to_srgb.apply(colour)));
        let held = hue_difference(source_lab, mapped_lab);

        assert!(in_gamut(mapped), "{colour:?} mapped to {mapped:?}");
        assert!(held < 2.0, "{colour:?} hue moved {held}°");
        assert!((source_lab[0] - mapped_lab[0]).abs() < 0.01, "{colour:?}");
        assert!(held <= hue_difference(source_lab, clipped_lab) + 0.5);
    }

    let green_clip_shift = hue_difference(
        oklab(ColorPrimaries::Bt2020, colours[1]),
        oklab(ColorPrimaries::Bt709, clip(to_srgb.apply(colours[1]))),
    );
    let untouched = widen(output[7]);
    let expected = to_srgb.apply(widen(texels[7]));

    assert!(green_clip_shift > 5.0, "{green_clip_shift}");
    for (found, expected) in untouched.iter().zip(expected) {
        assert!((found - expected).abs() < GAMUT_TOLERANCE);
    }
}

#[test]
fn a_source_that_matches_the_display_takes_no_mapping_pass() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let mut stage = stage(&gpu);
    let srgb = frame_color(ColorPrimaries::Bt709, TransferFunction::Srgb);
    let pq = frame_color(ColorPrimaries::Bt2020, TransferFunction::Pq);
    let hdr_display = ColorVolume {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        reference_white_nits: WHITE,
        peak_nits: 1000.0,
        black_nits: 0.0,
    };
    let codes = [0.1, 0.3, 0.5, 0.58, 0.65, 0.75];
    let texels: Vec<[f32; 4]> = codes.iter().map(|code| narrow([*code; 3])).collect();

    let sdr_route = stage
        .route(&gpu.device, srgb, ColorVolume::srgb_display())
        .unwrap();
    let (hdr_route, linear) = run(&gpu, &mut stage, pq, hdr_display, &texels);

    assert_eq!(sdr_route.passes(), 0);
    assert_eq!(
        sdr_route.output,
        srgb.mapped_to(&ColorVolume::srgb_display())
    );
    assert_eq!(sdr_route.output.space.primaries, srgb.space.primaries);
    assert!(hdr_route.linearise.is_some());
    assert!(hdr_route.map.is_none());
    for (texel, found) in texels.iter().zip(&linear) {
        let expected = pq_to_nits(f64::from(texel[0])) / WHITE;
        assert!((f64::from(found[0]) - expected).abs() <= 1e-3 + expected * 2e-3);
    }
}
