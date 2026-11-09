use super::*;

const DEBAND: DebandSettings = DebandSettings {
    iterations: 2,
    radius: 16,
    threshold: 3.0,
    chroma_threshold: 3.0,
};
const EIGHT_BIT_FULL_SCALE: f32 = 255.0;
const NEUTRAL_CHROMA: u8 = 128;
const UNCHANGED_TOLERANCE: f32 = 1.0e-3;
const RAMP_WIDTH: u32 = 256;
const RAMP_HEIGHT: u32 = 16;
const RAMP_BASE: u32 = 64;
const RAMP_BAND: u32 = 16;
const RAMP_MARGIN: u32 = 32;

fn luma_settings(size: (u32, u32)) -> RgbConvertSettings {
    RgbConvertSettings {
        range: ColorRange::Full,
        transfer: TransferFunction::Linear,
        output: OutputLight::Encoded,
        deband: Some(DEBAND),
        ..settings(
            size,
            ChromaSubsampling::Yuv444,
            BitDepth::Eight,
            PlaneLayout::Planar,
        )
    }
}

fn luma_frame(settings: &RgbConvertSettings, luma: impl Fn(u32, u32) -> u8) -> Vec<u8> {
    let layout = settings.frame_layout().unwrap();
    let mut bytes = vec![0; usize::try_from(layout.size).unwrap()];

    for (index, plane) in layout.planes.iter().enumerate() {
        for y in 0..plane.height {
            for x in 0..plane.width {
                let at = usize::try_from(plane.offset).unwrap() + (y * plane.stride + x) as usize;
                bytes[at] = if index == 0 {
                    luma(x, y)
                } else {
                    NEUTRAL_CHROMA
                };
            }
        }
    }
    bytes
}

fn luma_codes(image: &Image) -> Vec<Vec<f32>> {
    (0..i64::from(image.height))
        .map(|y| {
            (0..i64::from(image.width))
                .map(|x| image.texel(x, y)[0] * EIGHT_BIT_FULL_SCALE)
                .collect()
        })
        .collect()
}

fn ramp_code(x: u32) -> u8 {
    u8::try_from(RAMP_BASE + x / RAMP_BAND).unwrap()
}

fn ideal_ramp(x: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let position = (x as f32 + 0.5) / RAMP_BAND as f32;
    #[allow(clippy::cast_precision_loss)]
    let base = RAMP_BASE as f32;
    base + position - 0.5
}

struct RampError {
    mean: f32,
    largest_column_step: f32,
}

fn ramp_error(codes: &[Vec<f32>]) -> RampError {
    let columns = RAMP_MARGIN..RAMP_WIDTH - RAMP_MARGIN;
    #[allow(clippy::cast_precision_loss)]
    let rows = codes.len() as f32;
    let column_means: Vec<f32> = columns
        .clone()
        .map(|x| codes.iter().map(|row| row[x as usize]).sum::<f32>() / rows)
        .collect();

    #[allow(clippy::cast_precision_loss)]
    let samples = (codes.len() * column_means.len()) as f32;
    let mean = codes
        .iter()
        .flat_map(|row| {
            columns
                .clone()
                .map(|x| (row[x as usize] - ideal_ramp(x)).abs())
        })
        .sum::<f32>()
        / samples;
    let largest_column_step = column_means
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max);

    RampError {
        mean,
        largest_column_step,
    }
}

#[test]
fn deband_matches_reference_for_every_chroma_layout_and_filter() {
    let smooth = |width: u32, height: u32| {
        Image::from_fn(width, height, |x, y| {
            #[allow(clippy::cast_precision_loss)]
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            [0.2 + 0.6 * u, 0.3 + 0.4 * v, 0.5 + 0.15 * (u + v), 1.0]
        })
    };
    let chromas = [
        ChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444,
    ];
    let layouts = [PlaneLayout::Planar, PlaneLayout::SemiPlanar];

    for (index, (chroma, layout)) in chromas
        .into_iter()
        .flat_map(|chroma| layouts.map(|layout| (chroma, layout)))
        .enumerate()
    {
        let settings = RgbConvertSettings {
            deband: Some(DebandSettings {
                iterations: u32::try_from(index % 4).unwrap() + 1,
                radius: [4, 9, 16][index % 3],
                ..DEBAND
            }),
            chroma_filter: if index % 2 == 0 {
                ScaleFilter::Lanczos3
            } else {
                ScaleFilter::EwaLanczos
            },
            chroma_siting: if index % 3 == 0 {
                ChromaSiting::Center
            } else {
                ChromaSiting::Left
            },
            range: if index % 2 == 0 {
                ColorRange::Limited
            } else {
                ColorRange::Full
            },
            ..settings((48, 40), chroma, BitDepth::Eight, layout)
        };

        assert_image_matches_reference(&smooth(48, 40), settings);
        assert_matches_reference(RgbConvertSettings {
            format: PixelFormat {
                chroma,
                depth: BitDepth::Ten,
            },
            ..settings
        });
    }
}

#[test]
fn deband_matches_reference_through_the_fused_outputs() {
    assert_matches_reference(RgbConvertSettings {
        transfer: TransferFunction::Pq,
        output: OutputLight::Encoded,
        deband: Some(DEBAND),
        ..settings(
            (40, 24),
            ChromaSubsampling::Yuv420,
            BitDepth::Ten,
            PlaneLayout::SemiPlanar,
        )
    });
}

#[test]
fn deband_smooths_a_one_code_ramp() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let debanded = luma_settings((RAMP_WIDTH, RAMP_HEIGHT));
    let plain = RgbConvertSettings {
        deband: None,
        ..debanded
    };

    let bytes = luma_frame(&debanded, |x, _| ramp_code(x));
    let before = ramp_error(&luma_codes(&unpack_on_gpu(gpu, &bytes, plain)));
    let after = ramp_error(&luma_codes(&unpack_on_gpu(gpu, &bytes, debanded)));
    let expected = ramp_error(&luma_codes(&reference::unpack_rgb(&bytes, &debanded)));

    assert!(
        (before.largest_column_step - 1.0).abs() < 0.01,
        "{}",
        before.largest_column_step
    );
    assert!(
        after.largest_column_step < 0.25,
        "steps {} after debanding",
        after.largest_column_step
    );
    assert!(
        after.mean < before.mean / 2.0,
        "mean error {} before and {} after",
        before.mean,
        after.mean
    );
    assert!((after.mean - expected.mean).abs() < 0.01);
}

#[test]
fn deband_leaves_detail_at_the_threshold_untouched() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let debanded = luma_settings((96, 64));
    let plain = RgbConvertSettings {
        deband: None,
        ..debanded
    };
    let detail = |x: u32, y: u32| -> u8 {
        match x {
            0..32 if y == 20 => 105,
            0..32 => 100,
            32..64 => 120,
            _ if (x + y).is_multiple_of(2) => 84,
            _ => 80,
        }
    };

    let bytes = luma_frame(&debanded, detail);
    let unchanged = unpack_on_gpu(gpu, &bytes, plain);
    let filtered = unpack_on_gpu(gpu, &bytes, debanded);
    let expected = reference::unpack_rgb(&bytes, &debanded);

    assert!(
        filtered.max_difference(&unchanged) <= UNCHANGED_TOLERANCE,
        "{}",
        filtered.max_difference(&unchanged)
    );
    assert!(expected.max_difference(&unchanged) <= UNCHANGED_TOLERANCE);
}

#[test]
fn deband_keeps_an_unquantised_gradient() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let debanded = RgbConvertSettings {
        format: PixelFormat {
            chroma: ChromaSubsampling::Yuv444,
            depth: BitDepth::Twelve,
        },
        ..luma_settings((128, 32))
    };
    let plain = RgbConvertSettings {
        deband: None,
        ..debanded
    };
    let gradient = Image::from_fn(128, 32, |x, y| {
        #[allow(clippy::cast_precision_loss)]
        let level = 0.25 + 0.002 * x as f32 + 0.001 * y as f32;
        [level, level, level, 1.0]
    });

    let bytes = encode_on_gpu(gpu, &gradient, &debanded);
    let unchanged = unpack_on_gpu(gpu, &bytes, plain);
    let filtered = unpack_on_gpu(gpu, &bytes, debanded);

    assert!(
        interior_difference(&filtered, &unchanged, 16) <= RGBA16F_TOLERANCE / 4.0,
        "{}",
        interior_difference(&filtered, &unchanged, 16)
    );
}

#[test]
fn a_zero_threshold_turns_its_plane_off() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let debanded = RgbConvertSettings {
        deband: Some(DebandSettings {
            threshold: 0.0,
            chroma_threshold: 0.0,
            ..DEBAND
        }),
        ..luma_settings((RAMP_WIDTH, RAMP_HEIGHT))
    };
    let plain = RgbConvertSettings {
        deband: None,
        ..debanded
    };

    let bytes = luma_frame(&debanded, |x, _| ramp_code(x));
    let unchanged = unpack_on_gpu(gpu, &bytes, plain);
    let filtered = unpack_on_gpu(gpu, &bytes, debanded);

    assert_eq!(filtered, unchanged);
}

#[test]
fn deband_settings_are_validated_by_the_converter() {
    let settings = RgbConvertSettings {
        deband: Some(DebandSettings {
            radius: 0,
            ..DEBAND
        }),
        ..luma_settings((16, 16))
    };

    assert_eq!(
        settings.validate(),
        Err(ScaleError::DebandRadiusOutOfRange(0))
    );
}
