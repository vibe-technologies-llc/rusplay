use super::*;

const EIGHT_BIT_444: PixelFormat = PixelFormat {
    chroma: ChromaSubsampling::Yuv444,
    depth: BitDepth::Eight,
};

fn plane(width: usize, height: usize, sample: impl Fn(usize, usize) -> u16) -> SamplePlane {
    let samples = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| sample(x, y))
        .collect();

    SamplePlane::new(width, height, samples).unwrap()
}

fn picture(
    format: PixelFormat,
    width: usize,
    height: usize,
    luma: impl Fn(usize, usize) -> u16,
) -> SamplePicture {
    let (chroma_width, chroma_height) = chroma_size((width, height), format.chroma);

    SamplePicture::new(
        format,
        [
            plane(width, height, luma),
            plane(chroma_width, chroma_height, |_, _| 128),
            plane(chroma_width, chroma_height, |_, _| 128),
        ],
    )
    .unwrap()
}

fn texture(x: usize, y: usize) -> u16 {
    u16::try_from((x * 37 + y * 91 + (x * y) % 13) % 200 + 20).unwrap()
}

#[test]
fn identical_pictures_are_lossless() {
    let source = picture(EIGHT_BIT_444, 64, 48, texture);

    let score = measure(&source, &source, &[], TemporalOrder::First).unwrap();

    assert!(score.psnr_overall.is_identical());
    assert!(score.psnr.as_array().iter().all(|db| db.is_identical()));
    assert!(score.xpsnr.as_array().iter().all(|db| db.is_identical()));
    assert_eq!(score.psnr.luma.to_string(), "lossless");
}

#[test]
fn a_unit_error_everywhere_is_the_peak_ratio() {
    let source = picture(EIGHT_BIT_444, 64, 48, |_, _| 100);
    let reconstruction = picture(EIGHT_BIT_444, 64, 48, |_, _| 101);

    let score = measure(&source, &reconstruction, &[], TemporalOrder::First).unwrap();

    let expected = 10.0 * (255.0_f32 * 255.0).log10();
    assert!((score.psnr.luma.value() - expected).abs() < 0.001);
    assert!(score.psnr.cb.is_identical());
    let overall = 10.0 * (255.0_f32 * 255.0 * 3.0).log10();
    assert!((score.psnr_overall.value() - overall).abs() < 0.001);
}

#[test]
fn ten_bit_peak_is_used() {
    let format = PixelFormat {
        chroma: ChromaSubsampling::Yuv420,
        depth: BitDepth::Ten,
    };
    let source = picture(format, 64, 48, |_, _| 400);
    let reconstruction = picture(format, 64, 48, |_, _| 402);

    let score = measure(&source, &reconstruction, &[], TemporalOrder::First).unwrap();

    let expected = 10.0 * (1023.0_f32 * 1023.0 / 4.0).log10();
    assert!((score.psnr.luma.value() - expected).abs() < 0.001);
}

fn with_error_in(
    width: usize,
    height: usize,
    textured: impl Fn(usize, usize) -> bool,
    damaged: impl Fn(usize, usize) -> bool,
) -> QualityScore {
    let source_sample = |x, y| if textured(x, y) { texture(x, y) } else { 120 };
    let source = picture(EIGHT_BIT_444, width, height, source_sample);
    let reconstruction = picture(EIGHT_BIT_444, width, height, |x, y| {
        source_sample(x, y) + u16::from(damaged(x, y)) * 4
    });

    measure(&source, &reconstruction, &[], TemporalOrder::First).unwrap()
}

#[test]
fn error_in_texture_is_masked_and_error_in_flat_areas_is_not() {
    let half = |x: usize, _| x < 640;

    let in_texture = with_error_in(1280, 720, half, half);
    let in_flat = with_error_in(1280, 720, half, |x, _| x >= 640);

    assert!((in_texture.psnr.luma.value() - in_flat.psnr.luma.value()).abs() < 0.001);
    assert!(in_texture.xpsnr.luma.value() > in_flat.xpsnr.luma.value() + 3.0);
}

#[test]
fn motion_in_the_history_masks_error() {
    let source = picture(EIGHT_BIT_444, 320, 240, |_, _| 120);
    let reconstruction = picture(EIGHT_BIT_444, 320, 240, |x, y| {
        120 + u16::from((x + y) % 2 == 0)
    });
    let still = plane(320, 240, |_, _| 120);
    let moving = plane(320, 240, texture);

    let static_score = measure(&source, &reconstruction, &[&still], TemporalOrder::First).unwrap();
    let moving_score = measure(&source, &reconstruction, &[&moving], TemporalOrder::First).unwrap();

    assert!(moving_score.xpsnr.luma.value() > static_score.xpsnr.luma.value());
}

#[test]
fn second_order_uses_both_earlier_frames() {
    let source = picture(EIGHT_BIT_444, 320, 240, |_, _| 120);
    let reconstruction = picture(EIGHT_BIT_444, 320, 240, |x, _| 120 + u16::from(x % 3 == 0));
    let ramp = |offset: u16| plane(320, 240, move |_, _| 120 - offset);

    let linear = measure(
        &source,
        &reconstruction,
        &[&ramp(10), &ramp(20)],
        TemporalOrder::Second,
    )
    .unwrap();
    let first_order = measure(
        &source,
        &reconstruction,
        &[&ramp(10), &ramp(20)],
        TemporalOrder::First,
    )
    .unwrap();

    assert!(first_order.xpsnr.luma.value() > linear.xpsnr.luma.value());
}

#[test]
fn large_odd_pictures_take_the_downsampled_path() {
    let format = PixelFormat {
        chroma: ChromaSubsampling::Yuv420,
        depth: BitDepth::Ten,
    };
    let source = picture(format, 2561, 1441, |x, y| texture(x, y) * 4);
    let reconstruction = picture(format, 2561, 1441, |x, y| texture(x, y) * 4 + 3);
    let previous = plane(2561, 1441, |x, y| texture(x + 1, y) * 4);
    let before = plane(2561, 1441, |x, y| texture(x + 2, y) * 4);

    let score = measure(
        &source,
        &reconstruction,
        &[&previous, &before],
        TemporalOrder::Second,
    )
    .unwrap();

    assert!(score.xpsnr.luma.value().is_finite());
    assert!(score.xpsnr.cb.is_identical());
}

#[test]
fn tiny_pictures_fall_back_to_unweighted_error() {
    let source = picture(EIGHT_BIT_444, 8, 8, texture);
    let reconstruction = picture(EIGHT_BIT_444, 8, 8, |x, y| texture(x, y) + 2);

    let score = measure(&source, &reconstruction, &[], TemporalOrder::First).unwrap();

    assert_eq!(score.xpsnr.luma, score.psnr.luma);
}

#[test]
fn mismatched_pictures_are_refused() {
    let source = picture(EIGHT_BIT_444, 64, 48, texture);
    let other_size = picture(EIGHT_BIT_444, 64, 32, texture);
    let other_format = picture(
        PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth: BitDepth::Eight,
        },
        64,
        48,
        texture,
    );
    let short_history = plane(32, 48, texture);

    assert!(matches!(
        measure(&source, &other_size, &[], TemporalOrder::First),
        Err(QualityError::SizeMismatch { .. })
    ));
    assert!(matches!(
        measure(&source, &other_format, &[], TemporalOrder::First),
        Err(QualityError::FormatMismatch { .. })
    ));
    assert!(matches!(
        measure(&source, &source, &[&short_history], TemporalOrder::First),
        Err(QualityError::HistorySizeMismatch { .. })
    ));
}

#[test]
fn planes_are_checked_on_construction() {
    let luma = plane(64, 48, texture);
    let wrong_chroma = plane(64, 48, texture);

    assert!(matches!(
        SamplePlane::new(4, 4, vec![0; 15]),
        Err(QualityError::PlaneTooShort { needed: 16, .. })
    ));
    assert!(matches!(
        SamplePicture::new(
            PixelFormat {
                chroma: ChromaSubsampling::Yuv422,
                depth: BitDepth::Eight,
            },
            [luma, wrong_chroma.clone(), wrong_chroma],
        ),
        Err(QualityError::ChromaPlaneSize {
            plane: 1,
            expected: (32, 48),
            ..
        })
    ));
}

#[test]
fn temporal_order_follows_the_frame_rate() {
    assert_eq!(
        TemporalOrder::for_rate(FrameRate::integer(30)),
        TemporalOrder::First
    );
    assert_eq!(
        TemporalOrder::for_rate(FrameRate::new(60_000, 1001)),
        TemporalOrder::Second
    );
    assert_eq!(TemporalOrder::Second.history_frames(), 2);
}

#[test]
fn planes_read_from_padded_and_interleaved_bytes() {
    let padded = [1, 2, 9, 3, 4, 9];
    let wide = [0x40, 0x00, 0x80, 0x00, 0xc0, 0xff, 0x00, 0x01];

    let plane = SamplePlane::read(
        PlaneBytes {
            data: &padded,
            stride: 3,
            bytes_per_sample: 1,
            shift: 0,
        },
        2,
        2,
    )
    .unwrap();
    let (cb, cr) = SamplePlane::read_interleaved(
        PlaneBytes {
            data: &wide,
            stride: 8,
            bytes_per_sample: 2,
            shift: 6,
        },
        2,
        1,
    )
    .unwrap();

    assert_eq!(plane.samples(), &[1, 2, 3, 4]);
    assert_eq!(cb.samples(), &[1, 1023]);
    assert_eq!(cr.samples(), &[2, 4]);
    assert!(matches!(
        SamplePlane::read(
            PlaneBytes {
                data: &padded[..4],
                stride: 3,
                bytes_per_sample: 1,
                shift: 0,
            },
            2,
            2,
        ),
        Err(QualityError::PlaneBytesTooShort { needed: 5, .. })
    ));
}
