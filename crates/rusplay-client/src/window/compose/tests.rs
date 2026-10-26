use rusplay_core::{ColorPrimaries, ColorSpace, PixelFormat, TransferFunction, pq_from_nits};
use rusplay_scale::{ColorVolume, ScalePipelines};

use super::*;
use crate::{
    FrameColor, Gpu,
    present::{ColorStage, MappingOptions, Processing},
    testing,
    window::{
        dither::{bayer_rank, blue_noise_thresholds},
        output::{Dither, PQ_SIGNAL_REFERENCE_NITS},
    },
};

const WIDTH: u32 = 8;
const HEIGHT: u32 = 4;
const DITHER_SIZE: u32 = 64;
const EIGHT_BIT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const TEN_BIT: wgpu::TextureFormat = wgpu::TextureFormat::Rgb10a2Unorm;
const FLOAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn linear_encoding() -> DisplayEncoding {
    DisplayEncoding {
        target: ColorVolume::srgb_display(),
        primaries: ColorPrimaries::Bt709,
        signal: Signal::Linear,
        dither: None,
    }
}

fn hdr_volume() -> ColorVolume {
    ColorVolume {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        reference_white_nits: PQ_SIGNAL_REFERENCE_NITS,
        peak_nits: 1000.0,
        black_nits: 0.0,
    }
}

fn pq_encoding() -> DisplayEncoding {
    DisplayEncoding {
        target: hdr_volume(),
        primaries: ColorPrimaries::Bt2020,
        signal: Signal::Pq {
            white_nits: PQ_SIGNAL_REFERENCE_NITS,
        },
        dither: None,
    }
}

struct Picture<'a> {
    width: u32,
    height: u32,
    texels: &'a [[f32; 4]],
}

fn flat(width: u32, height: u32, texel: [f32; 4]) -> Vec<[f32; 4]> {
    vec![texel; (width * height) as usize]
}

fn render_view(
    gpu: &Gpu,
    encoding: &DisplayEncoding,
    picture: &wgpu::TextureView,
    size: (u32, u32),
    cursor: Option<([f32; 4], CursorPlacement)>,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    let target = testing::texture(
        gpu,
        size.0,
        size.1,
        format,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let cursor = cursor.map(|(texel, placement)| {
        let texels = flat(placement.size[0], placement.size[1], texel);
        (
            testing::upload(gpu, placement.size[0], placement.size[1], &texels)
                .create_view(&wgpu::TextureViewDescriptor::default()),
            placement,
        )
    });
    let mut composer = Composer::new(&gpu.device, &gpu.queue, encoding);
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    testing::submit(gpu, |encoder| {
        composer.encode(
            &gpu.device,
            &gpu.queue,
            encoder,
            picture,
            cursor.as_ref().map(|(view, placement)| (view, *placement)),
            &target_view,
            format,
        );
    });
    target
}

fn render(
    gpu: &Gpu,
    encoding: &DisplayEncoding,
    picture: &Picture<'_>,
    cursor: Option<([f32; 4], CursorPlacement)>,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    let view = testing::upload(gpu, picture.width, picture.height, picture.texels)
        .create_view(&wgpu::TextureViewDescriptor::default());

    render_view(
        gpu,
        encoding,
        &view,
        (picture.width, picture.height),
        cursor,
        format,
    )
}

fn composed_rgba8(gpu: &Gpu, cursor: Option<([f32; 4], CursorPlacement)>) -> Vec<u8> {
    let texels = flat(WIDTH, HEIGHT, [0.25, 0.25, 0.25, 1.0]);
    let picture = Picture {
        width: WIDTH,
        height: HEIGHT,
        texels: &texels,
    };

    testing::read_bytes(
        gpu,
        &render(gpu, &linear_encoding(), &picture, cursor, EIGHT_BIT),
    )
}

fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let start = ((y * WIDTH + x) * 4) as usize;
    bytes[start..start + 4].try_into().unwrap()
}

fn close(found: [u8; 4], expected: [u8; 4]) -> bool {
    found
        .iter()
        .zip(expected)
        .all(|(found, expected)| found.abs_diff(expected) <= 1)
}

fn srgb_oetf(linear: f64) -> f64 {
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_eotf(encoded: f64) -> f64 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn code(value: f64, steps: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let code = (value * steps).round() as u32;
    code
}

#[test]
fn the_picture_is_copied_one_to_one_without_a_cursor() {
    let Some(gpu) = testing::gpu() else {
        return;
    };

    let bytes = composed_rgba8(&gpu, None);

    for (x, y) in [(0, 0), (7, 3), (3, 1)] {
        assert!(close(pixel(&bytes, x, y), [64, 64, 64, 255]));
    }
}

#[test]
fn the_cursor_is_blended_premultiplied_inside_its_rectangle_only() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let placement = CursorPlacement {
        origin: [3, 1],
        size: [2, 2],
    };

    let bytes = composed_rgba8(&gpu, Some(([0.5, 0.0, 0.0, 0.5], placement)));

    assert!(close(pixel(&bytes, 3, 1), [159, 32, 32, 255]));
    assert!(close(pixel(&bytes, 4, 2), [159, 32, 32, 255]));
    assert!(close(pixel(&bytes, 2, 1), [64, 64, 64, 255]));
    assert!(close(pixel(&bytes, 5, 1), [64, 64, 64, 255]));
    assert!(close(pixel(&bytes, 3, 3), [64, 64, 64, 255]));
}

#[test]
fn a_cursor_partly_off_the_picture_is_clipped() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let placement = CursorPlacement {
        origin: [-1, -1],
        size: [2, 2],
    };

    let bytes = composed_rgba8(&gpu, Some(([1.0, 1.0, 1.0, 1.0], placement)));

    assert!(close(pixel(&bytes, 0, 0), [255, 255, 255, 255]));
    assert!(close(pixel(&bytes, 1, 0), [64, 64, 64, 255]));
    assert!(close(pixel(&bytes, 0, 1), [64, 64, 64, 255]));
}

#[test]
fn the_srgb_signal_matches_the_cpu_transfer_function() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let encoding = DisplayEncoding {
        signal: Signal::Srgb,
        ..linear_encoding()
    };
    let texels: Vec<[f32; 4]> = (0..64u8)
        .map(|step| {
            let value = testing::quantized(f32::from(step) / 63.0);
            [value, value * 0.5, value * value, 1.0]
        })
        .collect();
    let picture = Picture {
        width: 64,
        height: 1,
        texels: &texels,
    };

    let bytes = testing::read_bytes(&gpu, &render(&gpu, &encoding, &picture, None, EIGHT_BIT));

    for (texel, found) in texels.iter().zip(bytes.as_chunks::<4>().0) {
        for channel in 0..3 {
            let expected = code(srgb_oetf(f64::from(texel[channel])), 255.0);
            assert!(
                u32::from(found[channel]).abs_diff(expected) <= 1,
                "{texel:?}: {found:?}"
            );
        }
    }
}

#[test]
fn the_pq_signal_encodes_bt2020_light_at_its_reference_white() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let encoding = DisplayEncoding {
        target: ColorVolume {
            primaries: ColorPrimaries::DisplayP3,
            ..hdr_volume()
        },
        ..pq_encoding()
    };
    let to_bt2020 =
        ColorPrimaries::DisplayP3.rgb_to_rgb(ColorPrimaries::Bt2020, WhiteAdaptation::Bradford);
    let texels: Vec<[f32; 4]> = [0.0, 0.01, 0.1, 0.5, 1.0, 2.5, 4.9]
        .iter()
        .map(|value| {
            let value = testing::quantized(*value);
            [value, value * 0.7, testing::quantized(value * 0.2), 1.0]
        })
        .collect();
    let picture = Picture {
        width: 7,
        height: 1,
        texels: &texels,
    };

    let codes = testing::read_rgb10a2(&gpu, &render(&gpu, &encoding, &picture, None, TEN_BIT));

    for (texel, found) in texels.iter().zip(&codes) {
        let light = to_bt2020.apply([texel[0], texel[1], texel[2]].map(f64::from));
        for channel in 0..3 {
            let expected = code(
                pq_from_nits(light[channel].max(0.0) * PQ_SIGNAL_REFERENCE_NITS),
                1023.0,
            );
            assert!(
                found[channel].abs_diff(expected) <= 1,
                "{texel:?}: {found:?}"
            );
        }
    }
}

#[test]
fn extended_linear_keeps_wide_gamut_colours_as_negative_bt709_values() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let encoding = DisplayEncoding {
        target: ColorVolume {
            primaries: ColorPrimaries::DisplayP3,
            ..hdr_volume()
        },
        primaries: ColorPrimaries::Bt709,
        signal: Signal::ExtendedLinear,
        dither: None,
    };
    let to_bt709 =
        ColorPrimaries::DisplayP3.rgb_to_rgb(ColorPrimaries::Bt709, WhiteAdaptation::Bradford);
    let texels = [
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 3.0, 0.0, 1.0],
        [0.2, 0.2, 0.2, 1.0],
    ];
    let picture = Picture {
        width: 3,
        height: 1,
        texels: &texels,
    };
    let placement = CursorPlacement {
        origin: [2, 0],
        size: [1, 1],
    };

    let output = testing::read_rgba16f(
        &gpu,
        &render(
            &gpu,
            &encoding,
            &picture,
            Some(([0.0, 1.0, 0.0, 1.0], placement)),
            FLOAT,
        ),
    );

    for (texel, found) in texels.iter().zip(&output).take(2) {
        let expected = to_bt709.apply([texel[0], texel[1], texel[2]].map(f64::from));
        for channel in 0..3 {
            let found = f64::from(found[channel]);
            assert!((found - expected[channel]).abs() < 4e-3 * expected[channel].abs().max(1.0));
        }
    }
    assert!(output[0][1] < -0.03);
    assert!(output[1][1] > 3.0);
    for (found, expected) in output[2].iter().zip([0.0, 1.0, 0.0]) {
        assert!((f64::from(*found) - expected).abs() < 4e-3);
    }
}

fn dithered(gpu: &Gpu, pattern: Option<DitherPattern>, value: f32) -> Vec<u8> {
    let encoding = DisplayEncoding {
        dither: pattern.map(|pattern| Dither { pattern, bits: 8 }),
        ..linear_encoding()
    };
    let texels = flat(DITHER_SIZE, DITHER_SIZE, [value, value, value, 1.0]);
    let picture = Picture {
        width: DITHER_SIZE,
        height: DITHER_SIZE,
        texels: &texels,
    };

    testing::read_bytes(gpu, &render(gpu, &encoding, &picture, None, EIGHT_BIT))
        .as_chunks::<4>()
        .0
        .iter()
        .map(|texel| texel[0])
        .collect()
}

#[test]
fn dithering_spreads_a_level_between_codes_by_the_pattern_threshold() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let value = testing::quantized(100.3 / 255.0);
    let scaled = f64::from(value) * 255.0;
    let bayer_cells = f64::from(1u32 << (2 * BAYER_BITS));
    let noise = blue_noise_thresholds();

    let plain = dithered(&gpu, None, value);
    for pattern in [DitherPattern::Ordered, DitherPattern::BlueNoise] {
        let codes = dithered(&gpu, Some(pattern), value);
        let mut mismatched = 0;

        for (index, found) in codes.iter().enumerate() {
            let cell = u32::try_from(index).unwrap();
            let (x, y) = (cell % DITHER_SIZE, cell / DITHER_SIZE);
            let threshold = match pattern {
                DitherPattern::Ordered => (f64::from(bayer_rank(x, y)) + 0.5) / bayer_cells,
                DitherPattern::BlueNoise => f64::from(noise[index]),
            };
            let exact = scaled + threshold;
            let expected = exact.floor();
            if (f64::from(*found) - expected).abs() > 0.0 && (exact - exact.round()).abs() > 1e-3 {
                mismatched += 1;
            }
        }
        let mean = codes.iter().map(|code| f64::from(*code)).sum::<f64>()
            / f64::from(u32::try_from(codes.len()).unwrap());

        assert_eq!(mismatched, 0, "{pattern:?}");
        assert!(codes.iter().all(|code| *code == 100 || *code == 101));
        assert!(
            (mean - scaled).abs() < 0.01,
            "{pattern:?}: {mean} against {scaled}"
        );
    }
    assert!(plain.iter().all(|code| *code == 100));
}

#[test]
fn an_identity_chain_reproduces_the_source_codes() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut stage = ColorStage::new(
        &pipelines,
        MappingOptions {
            processing: Processing::Separate,
            ..MappingOptions::from(&rusplay_config::ColorConfig::default())
        },
    );
    let pq = FrameColor::of_stream(
        ColorSpace {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            ..ColorSpace::default()
        },
        PixelFormat::default(),
    );
    let pq_codes: Vec<u32> = (0..64).map(|step| step * 16).collect();
    let srgb_codes: Vec<u32> = (0..64).map(|step| step * 4).collect();
    let encoded: Vec<[f32; 4]> = pq_codes
        .iter()
        .map(|code| {
            let value = f64::from(*code) / 1023.0;
            #[allow(clippy::cast_possible_truncation)]
            let value = value as f32;
            [value, value, value, 1.0]
        })
        .collect();
    let srgb_linear: Vec<[f32; 4]> = srgb_codes
        .iter()
        .map(|code| {
            #[allow(clippy::cast_possible_truncation)]
            let value = srgb_eotf(f64::from(*code) / 255.0) as f32;
            [value, value, value, 1.0]
        })
        .collect();

    let route = stage.route(&gpu.device, pq, hdr_volume()).unwrap();
    let input = testing::upload(&gpu, 64, 1, &encoded);
    let linear = testing::storage(&gpu, 64, 1);
    let linear_view = linear.create_view(&wgpu::TextureViewDescriptor::default());
    testing::submit(&gpu, |encoder| {
        stage
            .linearise(
                &gpu.device,
                encoder,
                &input.create_view(&wgpu::TextureViewDescriptor::default()),
                &linear_view,
            )
            .unwrap();
    });
    let hdr = testing::read_rgb10a2(
        &gpu,
        &render_view(&gpu, &pq_encoding(), &linear_view, (64, 1), None, TEN_BIT),
    );
    let srgb_picture = Picture {
        width: 64,
        height: 1,
        texels: &srgb_linear,
    };
    let sdr_encoding = DisplayEncoding {
        signal: Signal::Srgb,
        ..linear_encoding()
    };
    let sdr = testing::read_bytes(
        &gpu,
        &render(&gpu, &sdr_encoding, &srgb_picture, None, EIGHT_BIT),
    );

    assert!(route.map.is_none());
    for (expected, found) in pq_codes.iter().zip(&hdr) {
        assert!(
            found.iter().all(|code| code.abs_diff(*expected) <= 1),
            "{expected}: {found:?}"
        );
    }
    for (expected, found) in srgb_codes.iter().zip(sdr.as_chunks::<4>().0) {
        assert!(
            found[..3]
                .iter()
                .all(|code| u32::from(*code).abs_diff(*expected) <= 1),
            "{expected}: {found:?}"
        );
    }
}
