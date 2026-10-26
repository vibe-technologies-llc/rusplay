use rusplay_core::{ColorPrimaries, GamutMapMode, pq_to_nits};

use super::*;
use crate::{
    ColorVolume, DynamicPeak, RgbConvertSettings, RgbConverter, ToneMapping, YuvFrame,
    color::{
        lut::tests::{
            Case, MATRIX, RANDOM_CODES, cases, grey_codes, hdr_levels, lit, mapping,
            measure_against_analytic, random_codes, saturated_codes, unpack_settings, volume,
        },
        reference,
    },
    testing::{self, Gpu, Image},
};

const GREY_DELTA_E: f64 = 0.5;
const MEAN_DELTA_E: f64 = 0.3;
const P99_DELTA_E: f64 = 1.0;
const MAX_DELTA_E: f64 = 3.0;
const DESATURATED_NEAR_PEAK: [f64; 3] = [0.632_136_8, 0.634_278_7, 0.328_276_3];
const REFERENCE_TOLERANCE: f64 = 0.005;

fn chain_settings(case: &Case) -> ColorChainSettings {
    ColorChainSettings {
        transfer: case.transfer,
        hdr: case.hdr,
        matrix: MATRIX,
        map: case.map,
    }
}

fn fused(gpu: &Gpu, case: &Case, converter: &mut RgbConverter, frame: &YuvFrame) -> Image {
    let pipelines = ScalePipelines::new(&gpu.device);
    let resolution = converter.settings().resolution;
    let mut chain = ColorChain::new(&pipelines, &gpu.device, chain_settings(case)).unwrap();
    let target = testing::storage_target(gpu, resolution.width, resolution.height);

    testing::submit(gpu, |encoder| {
        converter
            .encode_with_chain(
                &gpu.device,
                encoder,
                frame,
                &mut chain,
                &testing::view(&target),
            )
            .unwrap();
    });
    testing::download(gpu, &target)
}

#[test]
fn the_fused_chain_matches_the_analytic_passes() {
    let Some(gpu) = testing::gpu() else {
        return;
    };

    for case in cases() {
        let mut codes = random_codes(RANDOM_CODES);
        codes.extend(saturated_codes(case.transfer));
        let all = measure_against_analytic(gpu, &case, &lit(codes), |converter, frame| {
            fused(gpu, &case, converter, frame)
        });
        let greys = measure_against_analytic(gpu, &case, &lit(grey_codes()), |converter, frame| {
            fused(gpu, &case, converter, frame)
        });

        println!(
            "{}: delta e itp mean {:.3}, p99 {:.3}, max {:.3} at {:?}, greys max {:.3}",
            case.name, all.mean, all.p99, all.max, all.worst, greys.max
        );
        assert!(
            greys.max <= GREY_DELTA_E,
            "{}: greys {}",
            case.name,
            greys.max
        );
        assert!(all.mean <= MEAN_DELTA_E, "{}: mean {}", case.name, all.mean);
        assert!(all.p99 <= P99_DELTA_E, "{}: p99 {}", case.name, all.p99);
        let well_conditioned = case
            .map
            .is_none_or(|map| map.gamut.mode != GamutMapMode::Desaturate);
        assert!(
            !well_conditioned || all.max <= MAX_DELTA_E,
            "{}: max {} at {:?}",
            case.name,
            all.max,
            all.worst
        );
    }
}

#[test]
fn dynamic_peak_detection_is_not_fused() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let map = ColorMapSettings {
        tone: ToneMapping {
            dynamic_peak: Some(DynamicPeak::default()),
            ..ToneMapping::default()
        },
        ..mapping(
            volume(ColorPrimaries::Bt2020, TransferFunction::Pq, 4000.0),
            ColorVolume::srgb_display(),
            GamutMapMode::Perceptual,
        )
    };

    let refused = ColorChain::new(
        &pipelines,
        &gpu.device,
        ColorChainSettings {
            transfer: TransferFunction::Pq,
            hdr: hdr_levels(4000),
            matrix: MATRIX,
            map: Some(map),
        },
    )
    .err();

    assert_eq!(refused, Some(ScaleError::ChainDynamicPeak));
}

#[test]
fn a_chain_is_rebuilt_only_when_its_settings_change_and_must_match_the_frame() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let settings = ColorChainSettings {
        transfer: TransferFunction::Pq,
        hdr: hdr_levels(1000),
        matrix: MATRIX,
        map: None,
    };
    let mut chain = ColorChain::new(&pipelines, &gpu.device, settings).unwrap();
    let mut hlg = RgbConverter::new(
        &pipelines,
        &gpu.device,
        unpack_settings(1, TransferFunction::Hlg),
    )
    .unwrap();
    let mut linear = RgbConverter::new(
        &pipelines,
        &gpu.device,
        RgbConvertSettings {
            output: OutputLight::Linear,
            ..unpack_settings(1, TransferFunction::Srgb)
        },
    )
    .unwrap();
    let target = testing::view(&testing::storage_target(gpu, 1, 1));
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

    let unchanged = chain.configure(&gpu.device, settings).unwrap();
    let remapped = chain
        .configure(
            &gpu.device,
            ColorChainSettings {
                hdr: hdr_levels(4000),
                ..settings
            },
        )
        .unwrap();
    let mismatch = hlg.encode_with_chain(
        &gpu.device,
        &mut encoder,
        &hlg.create_frame(&gpu.device),
        &mut chain,
        &target,
    );
    let not_encoded = linear.encode_with_chain(
        &gpu.device,
        &mut encoder,
        &linear.create_frame(&gpu.device),
        &mut chain,
        &target,
    );

    assert_eq!(unchanged, Reconfigured::Unchanged);
    assert_eq!(remapped, Reconfigured::Tables);
    assert_eq!(
        mismatch,
        Err(ScaleError::ChainTransferMismatch {
            chain: TransferFunction::Pq,
            frame: TransferFunction::Hlg,
        })
    );
    assert_eq!(not_encoded, Err(ScaleError::ChainInputNotEncoded));
}

#[test]
fn the_fused_chain_follows_the_reference_where_an_intermediate_would_not() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let case = cases()
        .into_iter()
        .find(|case| {
            case.map
                .is_some_and(|map| map.gamut.mode == GamutMapMode::Desaturate)
        })
        .unwrap();
    let plan = ColorPlan::new(&case.map.unwrap()).unwrap();
    let light = DESATURATED_NEAR_PEAK.map(|code| pq_to_nits(code) / plan.source_white_nits);
    let expected = reference::map(&plan, light);
    let mut found = None;

    measure_against_analytic(gpu, &case, &[DESATURATED_NEAR_PEAK], |converter, frame| {
        let image = fused(gpu, &case, converter, frame);
        found = Some(image.pixels[0]);
        image
    });

    let found = found.unwrap();
    for channel in 0..3 {
        assert!(
            (f64::from(found[channel]) - expected[channel]).abs() <= REFERENCE_TOLERANCE,
            "{found:?} vs {expected:?}"
        );
    }
}
