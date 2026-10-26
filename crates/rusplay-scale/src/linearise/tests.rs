use rusplay_core::{ColorRange, PixelFormat, Resolution, ScaleFilter};

use super::*;
use crate::{
    ChromaSiting, ConvertSettings, InputLight, PlaneLayout,
    reference::{self, encode_linear, linearise_texel},
    testing::{self, Gpu, Image, RGBA16F_TOLERANCE},
};

const SIZE: (u32, u32) = (19, 11);

fn small(value: u32) -> f32 {
    f32::from(u16::try_from(value).unwrap())
}

fn ramp() -> Image {
    Image::from_fn(SIZE.0, SIZE.1, |x, y| {
        let step = |value: u32, len: u32| small(value) / small(len - 1);

        [
            step(x, SIZE.0),
            step(y, SIZE.1),
            step(x + y, SIZE.0 + SIZE.1),
            0.25,
        ]
    })
}

fn settings(transfer: TransferFunction) -> LineariseSettings {
    LineariseSettings {
        channels: SourceChannels::RGBA,
        transfer,
        hdr: HdrLevels::default(),
        matrix: MatrixCoefficients::Bt2020Ncl,
        output: OutputLight::Linear,
    }
}

fn run(gpu: &Gpu, settings: LineariseSettings, source: &wgpu::Texture) -> Image {
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut lineariser = Lineariser::new(&pipelines, &gpu.device, settings).unwrap();
    let target = testing::storage_target(gpu, source.width(), source.height());

    testing::submit(gpu, |encoder| {
        lineariser
            .encode(
                &gpu.device,
                encoder,
                &testing::view(source),
                &testing::view(&target),
            )
            .unwrap();
    });
    testing::download(gpu, &target)
}

fn relative_difference(left: &Image, right: &Image) -> f32 {
    left.pixels
        .iter()
        .zip(&right.pixels)
        .flat_map(|(a, b)| a.iter().zip(b))
        .map(|(a, b)| (a - b).abs() / b.abs().max(1.0))
        .fold(0.0, f32::max)
}

#[test]
fn every_transfer_matches_the_reference() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let image = ramp();
    let source = testing::upload(gpu, &image);

    for transfer in TransferFunction::ALL {
        let settings = settings(transfer);

        let got = run(gpu, settings, &source);
        let want = reference::linearise(&image, &settings);

        assert!(
            relative_difference(&got, &want) <= 2.0 * RGBA16F_TOLERANCE,
            "{transfer:?} differs by {}",
            relative_difference(&got, &want)
        );
    }
}

#[test]
fn pq_lands_on_absolute_luminance_over_the_reference_white() {
    let settings = settings(TransferFunction::Pq);
    let code = rusplay_core::pq_from_nits(1000.0);

    let [red, green, blue] = linearise_texel(&settings, [code, code, 0.0]);

    assert!((red - 1000.0 / 203.0).abs() < 1e-9);
    assert!((green - red).abs() < 1e-12);
    assert!(blue.abs() < 1e-12);
}

#[test]
fn linearising_inverts_the_converter_transfer() {
    for transfer in [
        TransferFunction::Srgb,
        TransferFunction::Bt709,
        TransferFunction::Gamma22,
        TransferFunction::Pq,
        TransferFunction::Hlg,
    ] {
        let hdr = HdrLevels {
            peak_nits: 1500,
            ..HdrLevels::default()
        };
        let convert = ConvertSettings {
            resolution: Resolution::new(2, 2),
            format: PixelFormat::default(),
            layout: PlaneLayout::Planar,
            matrix: MatrixCoefficients::Bt2020Ncl,
            range: ColorRange::Full,
            transfer,
            hdr,
            input: InputLight::Linear,
            chroma_filter: ScaleFilter::Hermite,
            chroma_siting: ChromaSiting::Left,
            row_alignment: 4,
        };
        let linearise = LineariseSettings {
            hdr,
            ..settings(transfer)
        };
        let peak = if transfer.is_hdr() {
            1500.0 / 203.0
        } else {
            1.0
        };

        for linear in [
            [0.0, 0.0, 0.0],
            [0.2, 0.5, 0.9],
            [peak, peak, peak],
            [0.5 * peak, 0.3 * peak, 0.1 * peak],
        ] {
            let encoded = encode_linear(&convert, linear);
            let back = linearise_texel(&linearise, encoded);

            for (got, want) in back.iter().zip(linear) {
                assert!(
                    (got - want).abs() <= 1e-4 * want.max(1.0),
                    "{transfer:?}: {linear:?} came back as {back:?}"
                );
            }
        }
    }
}

#[test]
fn channel_orders_pick_the_named_lanes() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let image = Image::from_fn(3, 2, |x, y| {
        let base = small(x + 3 * y) / 8.0;
        [base, base + 0.0625, base + 0.125, base + 0.1875]
    });
    let source = testing::upload(gpu, &image);

    for order in [
        ChannelOrder::Rgba,
        ChannelOrder::Bgra,
        ChannelOrder::Argb,
        ChannelOrder::Abgr,
    ] {
        for opaque in [false, true] {
            let settings = LineariseSettings::encoded(SourceChannels::float(order, opaque));

            let got = run(gpu, settings, &source);

            for (texel, lanes) in got.pixels.iter().zip(&image.pixels) {
                let pick = order.lanes().map(|lane| lanes[lane as usize]);
                let alpha = if opaque { 1.0 } else { pick[3] };
                assert_eq!(*texel, [pick[0], pick[1], pick[2], alpha], "{order:?}");
            }
        }
    }
}

fn packed_texture(gpu: &Gpu, words: &[u32], width: u32) -> wgpu::Texture {
    let height = u32::try_from(words.len()).unwrap() / width;
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rusplay-test-packed"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();

    gpu.queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: None,
        },
        texture.size(),
    );
    texture
}

fn rgbx1010102(red: u32, green: u32, blue: u32, alpha: u32) -> u32 {
    (red << 22) | (green << 12) | (blue << 2) | alpha
}

#[test]
fn packed_ten_bit_words_unpack_in_both_orders() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let channels = [
        (1023, 512, 0, 3),
        (4, 300, 1000, 1),
        (0, 0, 1, 0),
        (77, 1, 900, 2),
    ];
    let words: Vec<u32> = channels
        .iter()
        .map(|&(red, green, blue, alpha)| rgbx1010102(red, green, blue, alpha))
        .collect();
    let source = packed_texture(gpu, &words, 2);
    let ten = |code: u32| small(code) / 1023.0;

    let rgbx = run(
        gpu,
        LineariseSettings::encoded(SourceChannels::packed(ChannelOrder::Bgra, false)),
        &source,
    );
    let bgrx = run(
        gpu,
        LineariseSettings::encoded(SourceChannels::packed(ChannelOrder::Rgba, true)),
        &source,
    );

    for ((rgb, bgr), &(red, green, blue, alpha)) in
        rgbx.pixels.iter().zip(&bgrx.pixels).zip(&channels)
    {
        let two = small(alpha) / 3.0;
        let close = |got: &[f32; 4], want: [f32; 4]| {
            got.iter()
                .zip(want)
                .all(|(got, want)| (got - want).abs() <= RGBA16F_TOLERANCE / 2.0)
        };
        assert!(
            close(rgb, [ten(red), ten(green), ten(blue), two]),
            "{rgb:?}"
        );
        assert!(
            close(bgr, [ten(blue), ten(green), ten(red), 1.0]),
            "{bgr:?}"
        );
    }
}

#[test]
fn a_source_of_the_wrong_kind_is_refused() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut lineariser = Lineariser::new(
        &pipelines,
        &gpu.device,
        LineariseSettings::encoded(SourceChannels::packed(ChannelOrder::Rgba, true)),
    )
    .unwrap();
    let source = testing::upload(gpu, &ramp());
    let target = testing::storage_target(gpu, SIZE.0, SIZE.1);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let result = lineariser.encode(
        &gpu.device,
        &mut encoder,
        &testing::view(&source),
        &testing::view(&target),
    );

    assert_eq!(
        result,
        Err(ScaleError::LineariseSourceFormat {
            format: wgpu::TextureFormat::Rgba16Float,
            packing: TexelPacking::Rgb10Alpha2Low,
        })
    );
}

#[test]
fn hdr_levels_are_validated_and_reconfiguring_reports_parameters() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut lineariser =
        Lineariser::new(&pipelines, &gpu.device, settings(TransferFunction::Srgb)).unwrap();
    let broken = LineariseSettings {
        hdr: HdrLevels {
            reference_white_nits: 0,
            ..HdrLevels::default()
        },
        ..settings(TransferFunction::Pq)
    };

    assert_eq!(
        lineariser.configure(&gpu.device, settings(TransferFunction::Srgb)),
        Ok(Reconfigured::Unchanged)
    );
    assert_eq!(
        lineariser.configure(&gpu.device, settings(TransferFunction::Pq)),
        Ok(Reconfigured::Parameters)
    );
    assert_eq!(
        lineariser.configure(&gpu.device, broken),
        Err(ScaleError::ReferenceWhiteOutOfRange(0))
    );
}
