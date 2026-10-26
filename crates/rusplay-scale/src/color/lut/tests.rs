use rusplay_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, GamutMapMode, PixelFormat, Resolution,
    ScaleFilter, WhiteAdaptation, pq_from_nits,
};

use super::*;
use crate::{
    ChromaSiting, ColorMapper, ColorVolume, GamutMapping, Lineariser, PlaneLayout, Quantization,
    RgbConvertSettings, RgbConverter, ToneMapping, YuvFrame,
    testing::{self, Gpu, Image},
};

const WIDTH: u32 = 256;
const RANDOM_CODES: usize = 1 << 17;
const RANDOM_SEED: u64 = 0x9e37_79b9_7f4a_7c15;
const GREY_STEPS: u32 = 1023;
const LIT_CODE: f64 = 0.03;
const TIGHT_DELTA_E: f64 = 1.0;
const MEAN_DELTA_E: f64 = 0.3;
const P99_DELTA_E: f64 = 3.0;
const MATRIX: MatrixCoefficients = MatrixCoefficients::Bt2020Ncl;

struct Case {
    name: &'static str,
    transfer: TransferFunction,
    hdr: HdrLevels,
    map: Option<ColorMapSettings>,
    smooth: bool,
}

impl Case {
    fn target(&self) -> ColorVolume {
        self.map.map_or(
            ColorVolume {
                primaries: ColorPrimaries::Bt2020,
                transfer: self.transfer,
                reference_white_nits: f64::from(self.hdr.reference_white_nits),
                peak_nits: f64::from(self.hdr.peak_nits),
                black_nits: 0.0,
            },
            |map| map.target,
        )
    }
}

fn volume(primaries: ColorPrimaries, transfer: TransferFunction, peak_nits: f64) -> ColorVolume {
    ColorVolume {
        primaries,
        transfer,
        peak_nits,
        ..ColorVolume::srgb_display()
    }
}

fn mapping(source: ColorVolume, target: ColorVolume, mode: GamutMapMode) -> ColorMapSettings {
    ColorMapSettings {
        source,
        target,
        tone: ToneMapping::default(),
        gamut: GamutMapping {
            mode,
            ..GamutMapping::default()
        },
    }
}

fn hdr_levels(peak_nits: u32) -> HdrLevels {
    HdrLevels {
        peak_nits,
        ..HdrLevels::default()
    }
}

fn cases() -> Vec<Case> {
    let pq = |peak| volume(ColorPrimaries::Bt2020, TransferFunction::Pq, peak);
    let hlg = volume(ColorPrimaries::Bt2020, TransferFunction::Hlg, 1000.0);
    let srgb = ColorVolume::srgb_display();
    let p3 = volume(ColorPrimaries::DisplayP3, TransferFunction::Srgb, 203.0);
    let p3_hdr = volume(ColorPrimaries::DisplayP3, TransferFunction::Pq, 600.0);
    let bt2020_sdr = volume(ColorPrimaries::Bt2020, TransferFunction::Srgb, 203.0);

    vec![
        Case {
            name: "pq 4000 bt2020 -> srgb perceptual",
            transfer: TransferFunction::Pq,
            hdr: hdr_levels(4000),
            map: Some(mapping(pq(4000.0), srgb, GamutMapMode::Perceptual)),
            smooth: false,
        },
        Case {
            name: "pq 1000 bt2020 -> p3 sdr perceptual",
            transfer: TransferFunction::Pq,
            hdr: hdr_levels(1000),
            map: Some(mapping(pq(1000.0), p3, GamutMapMode::Perceptual)),
            smooth: false,
        },
        Case {
            name: "pq 4000 bt2020 -> p3 pq 600 perceptual",
            transfer: TransferFunction::Pq,
            hdr: hdr_levels(4000),
            map: Some(mapping(pq(4000.0), p3_hdr, GamutMapMode::Perceptual)),
            smooth: false,
        },
        Case {
            name: "pq 1000 bt2020 -> srgb relative",
            transfer: TransferFunction::Pq,
            hdr: hdr_levels(1000),
            map: Some(mapping(pq(1000.0), srgb, GamutMapMode::Relative)),
            smooth: false,
        },
        Case {
            name: "pq 1000 bt2020 -> srgb desaturate",
            transfer: TransferFunction::Pq,
            hdr: hdr_levels(1000),
            map: Some(mapping(pq(1000.0), srgb, GamutMapMode::Desaturate)),
            smooth: false,
        },
        Case {
            name: "hlg 1000 bt2020 -> p3 sdr perceptual",
            transfer: TransferFunction::Hlg,
            hdr: hdr_levels(1000),
            map: Some(mapping(hlg, p3, GamutMapMode::Perceptual)),
            smooth: false,
        },
        Case {
            name: "hlg 1000 bt2020 -> p3 pq 600 perceptual",
            transfer: TransferFunction::Hlg,
            hdr: hdr_levels(1000),
            map: Some(mapping(hlg, p3_hdr, GamutMapMode::Perceptual)),
            smooth: false,
        },
        Case {
            name: "srgb bt2020 -> srgb perceptual",
            transfer: TransferFunction::Srgb,
            hdr: HdrLevels::default(),
            map: Some(mapping(bt2020_sdr, srgb, GamutMapMode::Perceptual)),
            smooth: false,
        },
        Case {
            name: "srgb bt2020 -> p3 relative",
            transfer: TransferFunction::Srgb,
            hdr: HdrLevels::default(),
            map: Some(mapping(bt2020_sdr, p3, GamutMapMode::Relative)),
            smooth: true,
        },
        Case {
            name: "pq 1000 bt2020 linearised only",
            transfer: TransferFunction::Pq,
            hdr: hdr_levels(1000),
            map: None,
            smooth: true,
        },
        Case {
            name: "hlg 1000 bt2020 linearised only",
            transfer: TransferFunction::Hlg,
            hdr: hdr_levels(1000),
            map: None,
            smooth: true,
        },
    ]
}

fn saturated_codes(transfer: TransferFunction) -> Vec<[f64; 3]> {
    let encode = |nits: f64| match transfer {
        TransferFunction::Pq => pq_from_nits(nits),
        _ => (nits / 1000.0).powf(1.0 / 2.4),
    };
    let directions = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 1.0],
        [1.0, 0.0, 1.0],
        [1.0, 1.0, 0.0],
        [1.0, 0.1, 0.05],
        [0.05, 1.0, 0.2],
        [0.1, 0.2, 1.0],
    ];
    let levels = [
        1.0, 20.0, 100.0, 203.0, 400.0, 600.0, 1000.0, 2000.0, 4000.0,
    ];

    directions
        .iter()
        .flat_map(|direction| {
            levels.map(|nits| direction.map(|weight| encode(weight * nits).clamp(0.0, 1.0)))
        })
        .collect()
}

fn random_codes(count: usize) -> Vec<[f64; 3]> {
    let mut state = RANDOM_SEED;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        f64::from(u32::try_from(state >> 40).unwrap()) / f64::from(1_u32 << 24)
    };

    (0..count).map(|_| [next(), next(), next()]).collect()
}

fn grey_codes() -> Vec<[f64; 3]> {
    (0..=GREY_STEPS)
        .map(|step| [f64::from(step) / f64::from(GREY_STEPS); 3])
        .collect()
}

fn unpack_settings(rows: u32, transfer: TransferFunction) -> RgbConvertSettings {
    RgbConvertSettings {
        resolution: Resolution::new(WIDTH, rows),
        format: PixelFormat {
            chroma: ChromaSubsampling::Yuv444,
            depth: BitDepth::Twelve,
        },
        layout: PlaneLayout::Planar,
        matrix: MATRIX,
        range: ColorRange::Full,
        transfer,
        output: OutputLight::Encoded,
        chroma_filter: ScaleFilter::Hermite,
        chroma_siting: ChromaSiting::Center,
        row_alignment: 4,
    }
}

fn frame_bytes(converter: &RgbConverter, codes: &[[f64; 3]]) -> Vec<u8> {
    let layout = converter.layout();
    let quantization = Quantization::new(BitDepth::Twelve, ColorRange::Full);
    let rows = MATRIX.rows();
    let mut bytes = vec![0_u8; usize::try_from(layout.size).unwrap()];

    for (index, rgb) in codes.iter().enumerate() {
        let ycbcr = rows.map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]);
        let samples = [
            ycbcr[0] * quantization.luma.0 + quantization.luma.1,
            ycbcr[1] * quantization.chroma.0 + quantization.chroma.1,
            ycbcr[2] * quantization.chroma.0 + quantization.chroma.1,
        ];
        let (x, y) = (index % WIDTH as usize, index / WIDTH as usize);

        for (plane, sample) in layout.planes.iter().zip(samples) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let code = sample.round().clamp(0.0, f64::from(quantization.max_code)) as u16;
            let start = usize::try_from(plane.offset).unwrap() + y * plane.stride as usize + 2 * x;
            bytes[start..start + 2].copy_from_slice(&code.to_le_bytes());
        }
    }
    bytes
}

fn analytic(gpu: &Gpu, case: &Case, converter: &mut RgbConverter, frame: &YuvFrame) -> Image {
    let pipelines = ScalePipelines::new(&gpu.device);
    let size = converter.settings().resolution;
    let linearise = LineariseSettings {
        channels: SourceChannels::RGBA,
        transfer: case.transfer,
        hdr: case.hdr,
        matrix: MATRIX,
        output: OutputLight::Linear,
    };
    let mut lineariser = Lineariser::new(&pipelines, &gpu.device, linearise).unwrap();
    let mut mapper = case
        .map
        .map(|map| ColorMapper::new(&pipelines, &gpu.device, map).unwrap());
    let textures = [(); 3].map(|()| testing::storage_target(gpu, size.width, size.height));
    let views = textures.each_ref().map(testing::view);

    testing::submit(gpu, |encoder| {
        converter
            .encode(&gpu.device, encoder, frame, &views[0])
            .unwrap();
        lineariser
            .encode(&gpu.device, encoder, &views[0], &views[1])
            .unwrap();
        if let Some(mapper) = &mut mapper {
            mapper.encode(&gpu.device, encoder, &views[1], &views[2]);
        }
    });
    testing::download(gpu, &textures[1 + usize::from(case.map.is_some())])
}

fn fused(
    gpu: &Gpu,
    case: &Case,
    size: u32,
    converter: &mut RgbConverter,
    frame: &YuvFrame,
) -> Image {
    let pipelines = ScalePipelines::new(&gpu.device);
    let resolution = converter.settings().resolution;
    let settings = ColorLutSettings {
        transfer: case.transfer,
        hdr: case.hdr,
        matrix: MATRIX,
        map: case.map,
        size,
    };
    let mut lut = ColorLut::new(&pipelines, &gpu.device, settings).unwrap();
    let target = testing::storage_target(gpu, resolution.width, resolution.height);

    testing::submit(gpu, |encoder| {
        converter
            .encode_with_lut(
                &gpu.device,
                encoder,
                frame,
                &mut lut,
                &testing::view(&target),
            )
            .unwrap();
    });
    testing::download(gpu, &target)
}

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
const DELTA_E_ITP_SCALE: f64 = 720.0;

fn apply(rows: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    rows.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

fn itp(target: &ColorVolume, texel: [f32; 4]) -> [f64; 3] {
    let white = target.reference_white_nits;
    let nits = [texel[0], texel[1], texel[2]].map(|channel| f64::from(channel) * white);
    let bt2020 = target
        .primaries
        .rgb_to_rgb(ColorPrimaries::Bt2020, WhiteAdaptation::Bradford)
        .apply(nits);
    let lms =
        apply(ICTCP_LMS, bt2020).map(|channel| channel.signum() * pq_from_nits(channel.abs()));
    let [i, ct, cp] = apply(ICTCP_ITP, lms);
    [i, 0.5 * ct, cp]
}

fn delta_e_itp(target: &ColorVolume, expected: [f32; 4], found: [f32; 4]) -> f64 {
    let (a, b) = (itp(target, expected), itp(target, found));
    DELTA_E_ITP_SCALE
        * (0..3)
            .map(|axis| (a[axis] - b[axis]).powi(2))
            .sum::<f64>()
            .sqrt()
}

struct Accuracy {
    mean: f64,
    p99: f64,
    max: f64,
    worst: [f64; 3],
}

impl Accuracy {
    fn of(errors: &[f64], codes: &[[f64; 3]]) -> Self {
        let mut sorted = errors.to_vec();
        sorted.sort_by(f64::total_cmp);
        let (worst, max) = errors
            .iter()
            .copied()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .unwrap_or_default();
        #[allow(clippy::cast_precision_loss)]
        let mean = errors.iter().sum::<f64>() / errors.len() as f64;

        Self {
            mean,
            p99: sorted[sorted.len() * 99 / 100],
            max,
            worst: codes[worst],
        }
    }
}

fn measure(gpu: &Gpu, case: &Case, size: u32, codes: &[[f64; 3]]) -> Accuracy {
    let pipelines = ScalePipelines::new(&gpu.device);
    let rows = u32::try_from(codes.len().div_ceil(WIDTH as usize)).unwrap();
    let mut converter = RgbConverter::new(
        &pipelines,
        &gpu.device,
        unpack_settings(rows, case.transfer),
    )
    .unwrap();
    let frame = converter.create_frame(&gpu.device);

    gpu.queue
        .write_buffer(frame.buffer(), 0, &frame_bytes(&converter, codes));
    let expected = analytic(gpu, case, &mut converter, &frame);
    let found = fused(gpu, case, size, &mut converter, &frame);
    let errors: Vec<f64> = expected
        .pixels
        .iter()
        .zip(&found.pixels)
        .take(codes.len())
        .map(|(expected, found)| delta_e_itp(&case.target(), *expected, *found))
        .collect();

    Accuracy::of(&errors, codes)
}

fn lit(codes: impl IntoIterator<Item = [f64; 3]>) -> Vec<[f64; 3]> {
    codes
        .into_iter()
        .filter(|code| code.iter().any(|channel| *channel >= LIT_CODE))
        .collect()
}

#[test]
fn lut_matches_the_analytic_passes() {
    let Some(gpu) = testing::gpu() else {
        return;
    };

    for case in cases() {
        let mut codes = random_codes(RANDOM_CODES);
        codes.extend(saturated_codes(case.transfer));
        let all = measure(gpu, &case, DEFAULT_LUT_SIZE, &lit(codes));
        let greys = measure(gpu, &case, DEFAULT_LUT_SIZE, &lit(grey_codes()));

        println!(
            "{}: delta e itp mean {:.3}, p99 {:.3}, max {:.3} at {:?}, greys max {:.3}",
            case.name, all.mean, all.p99, all.max, all.worst, greys.max
        );
        assert!(
            greys.max <= TIGHT_DELTA_E,
            "{}: greys {}",
            case.name,
            greys.max
        );
        assert!(all.mean <= MEAN_DELTA_E, "{}: mean {}", case.name, all.mean);
        assert!(all.p99 <= P99_DELTA_E, "{}: p99 {}", case.name, all.p99);
        if case.smooth {
            assert!(
                all.max <= TIGHT_DELTA_E,
                "{}: max {} at {:?}",
                case.name,
                all.max,
                all.worst
            );
        }
    }
}

#[test]
fn unbakeable_settings_are_refused() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let map = mapping(
        volume(ColorPrimaries::Bt2020, TransferFunction::Pq, 4000.0),
        ColorVolume::srgb_display(),
        GamutMapMode::Perceptual,
    );
    let base = ColorLutSettings {
        transfer: TransferFunction::Pq,
        hdr: hdr_levels(4000),
        matrix: MATRIX,
        map: Some(map),
        size: DEFAULT_LUT_SIZE,
    };
    let dynamic = ColorMapSettings {
        tone: ToneMapping {
            dynamic_peak: Some(crate::DynamicPeak::default()),
            ..ToneMapping::default()
        },
        ..map
    };
    let highlight = mapping(map.source, map.target, GamutMapMode::Highlight);
    let refused = |settings| ColorLut::new(&pipelines, &gpu.device, settings).err();

    assert_eq!(
        refused(ColorLutSettings { size: 1, ..base }),
        Some(ScaleError::LutSizeOutOfRange(1))
    );
    assert_eq!(
        refused(ColorLutSettings {
            map: Some(dynamic),
            ..base
        }),
        Some(ScaleError::LutDynamicPeak)
    );
    assert_eq!(
        refused(ColorLutSettings {
            map: Some(highlight),
            ..base
        }),
        Some(ScaleError::LutHighlightMapping)
    );
}

#[test]
fn a_lut_is_rebuilt_only_when_its_settings_change_and_must_match_the_frame() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let settings = ColorLutSettings {
        transfer: TransferFunction::Pq,
        hdr: hdr_levels(1000),
        matrix: MATRIX,
        map: None,
        size: DEFAULT_LUT_SIZE,
    };
    let mut lut = ColorLut::new(&pipelines, &gpu.device, settings).unwrap();
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
            transfer: TransferFunction::Srgb,
            ..unpack_settings(1, TransferFunction::Srgb)
        },
    )
    .unwrap();
    let target = testing::view(&testing::storage_target(gpu, WIDTH, 1));
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());

    let unchanged = lut.configure(&gpu.device, settings).unwrap();
    let resized = lut
        .configure(
            &gpu.device,
            ColorLutSettings {
                size: 33,
                ..settings
            },
        )
        .unwrap();
    let mismatch = hlg.encode_with_lut(
        &gpu.device,
        &mut encoder,
        &hlg.create_frame(&gpu.device),
        &mut lut,
        &target,
    );
    let not_encoded = linear.encode_with_lut(
        &gpu.device,
        &mut encoder,
        &linear.create_frame(&gpu.device),
        &mut lut,
        &target,
    );

    assert_eq!(unchanged, Reconfigured::Unchanged);
    assert_eq!(resized, Reconfigured::Tables);
    assert_eq!(
        mismatch,
        Err(ScaleError::LutTransferMismatch {
            lut: TransferFunction::Pq,
            frame: TransferFunction::Hlg,
        })
    );
    assert_eq!(not_encoded, Err(ScaleError::LutInputNotEncoded));
}
