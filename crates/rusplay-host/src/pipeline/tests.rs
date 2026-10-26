mod handoff;

use std::os::fd::OwnedFd;

use rusplay_capture::{BufferLease, DRM_FORMAT_MOD_LINEAR, DrmFourcc};
use rusplay_core::{BitDepth, ChromaSubsampling, ColorPrimaries, TransferFunction, pq_from_nits};
use rusplay_scale::{ScaleError, udmabuf};

use super::*;

const SIZE: Resolution = Resolution::new(16, 8);
const TEN_BIT_MAX: f64 = 1023.0;
const EIGHT_BIT_MAX: f64 = 255.0;
const DMABUF_STRIDE: u32 = 256;

fn config(color: ColorSpace, depth: BitDepth) -> Config {
    let mut config = Config::default();
    config.video.color = color;
    config.video.pixel_format = PixelFormat {
        chroma: ChromaSubsampling::Yuv444,
        depth,
    };
    config
}

fn pipeline(config: Config) -> Option<GpuPipeline> {
    let gpu = Gpu::open().ok()?;

    Some(GpuPipeline::new(gpu, config, None))
}

fn dmabuf_pipeline(config: Config) -> Option<GpuPipeline> {
    let pipeline = pipeline(config)?;
    DmaBufImporter::new(&pipeline.gpu.device).ok()?;
    udmabuf::create(&[0])?;

    Some(pipeline)
}

fn srgb() -> ColorSpace {
    ColorSpace {
        transfer: TransferFunction::Srgb,
        ..ColorSpace::default()
    }
}

fn pq_bt2020() -> ColorSpace {
    ColorSpace {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        ..ColorSpace::default()
    }
}

fn frame(fourcc: [u8; 4], color: ColorSpace, texel: impl Fn(u32) -> u32) -> Frame {
    let stride = SIZE.width * 4;
    let data = (0..SIZE.height)
        .flat_map(|_| (0..SIZE.width).map(&texel))
        .flat_map(u32::to_le_bytes)
        .collect();

    Frame {
        resolution: SIZE,
        format: DrmFourcc::from_code(fourcc),
        color,
        timestamp: Timestamp::ZERO,
        buffer: FrameBuffer::Shm { data, stride },
    }
}

fn dmabuf_frame(fourcc: [u8; 4], color: ColorSpace, texel: impl Fn(u32) -> u32) -> Frame {
    let row_padding = (DMABUF_STRIDE - SIZE.width * 4) as usize;
    let bytes: Vec<u8> = (0..SIZE.height)
        .flat_map(|_| {
            (0..SIZE.width)
                .map(&texel)
                .flat_map(u32::to_le_bytes)
                .chain(std::iter::repeat_n(0, row_padding))
        })
        .collect();

    Frame {
        resolution: SIZE,
        format: DrmFourcc::from_code(fourcc),
        color,
        timestamp: Timestamp::ZERO,
        buffer: FrameBuffer::DmaBuf {
            fd: udmabuf::create(&bytes).unwrap(),
            modifier: DRM_FORMAT_MOD_LINEAR,
            planes: vec![Plane {
                offset: 0,
                stride: DMABUF_STRIDE,
            }],
            lease: BufferLease::detached(),
        },
    }
}

fn unimportable_frame() -> Frame {
    Frame {
        resolution: SIZE,
        format: DrmFourcc::from_code(*b"YUYV"),
        color: srgb(),
        timestamp: Timestamp::ZERO,
        buffer: FrameBuffer::DmaBuf {
            fd: OwnedFd::from(tempfile::tempfile().unwrap()),
            modifier: DRM_FORMAT_MOD_LINEAR,
            planes: vec![Plane {
                offset: 0,
                stride: DMABUF_STRIDE,
            }],
            lease: BufferLease::detached(),
        },
    }
}

fn xbgr8888(red: u32, green: u32, blue: u32) -> u32 {
    (0xff << 24) | (blue << 16) | (green << 8) | red
}

fn bgrx8888(red: u32, green: u32, blue: u32) -> u32 {
    (blue << 24) | (green << 16) | (red << 8) | 0xff
}

fn xrgb8888(red: u32, green: u32, blue: u32) -> u32 {
    (0xff << 24) | (red << 16) | (green << 8) | blue
}

fn xrgb2101010(red: u32, green: u32, blue: u32) -> u32 {
    (3 << 30) | (red << 20) | (green << 10) | blue
}

fn ten_bit_code(value: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let code = (value * TEN_BIT_MAX).round() as u32;
    code
}

fn luma_row(pipeline: &mut GpuPipeline, frame: &Frame) -> Vec<u32> {
    let mut source = pipeline.upload(frame).unwrap().unwrap();
    pipeline.convert(&mut source).unwrap();
    pipeline.read_back(frame.timestamp).unwrap();

    let input = pipeline.input.as_ref().unwrap();
    let FrameBuffer::Shm { data, .. } = &input.buffer else {
        unreachable!("the encoder input is always shared memory");
    };
    let sample = match pipeline.config.video.pixel_format.depth {
        BitDepth::Eight => 1,
        BitDepth::Ten | BitDepth::Twelve => 2,
    };

    data.chunks_exact(sample)
        .take(input.resolution.width as usize)
        .map(|bytes| match bytes {
            [low] => u32::from(*low),
            [low, high] => u32::from(u16::from_le_bytes([*low, *high])),
            _ => unreachable!("samples are one or two bytes"),
        })
        .collect()
}

fn assert_codes(row: &[u32], expected: u32, slack: u32, what: &str) {
    for code in row {
        assert!(
            code.abs_diff(expected) <= slack,
            "{what}: code {code}, expected {expected}"
        );
    }
}

#[test]
fn a_ten_bit_capture_in_the_stream_encoding_keeps_its_codes_and_channel_order() {
    let Some(mut pipeline) = pipeline(config(pq_bt2020(), BitDepth::Ten)) else {
        return;
    };

    let grey = luma_row(
        &mut pipeline,
        &frame(*b"XR30", pq_bt2020(), |_| xrgb2101010(520, 520, 520)),
    );
    let red = luma_row(
        &mut pipeline,
        &frame(*b"XR30", pq_bt2020(), |_| xrgb2101010(1023, 0, 0)),
    );
    let blue_first = luma_row(
        &mut pipeline,
        &frame(*b"XB30", pq_bt2020(), |_| xrgb2101010(0, 0, 1023)),
    );

    assert_codes(&grey, 520, 0, "grey");
    assert_codes(&red, ten_bit_code(0.2627), 1, "red");
    assert_codes(&blue_first, ten_bit_code(0.2627), 1, "xb30 red");
}

#[test]
fn an_hdr_capture_is_linearised_and_mapped_into_an_sdr_stream() {
    let Some(mut pipeline) = pipeline(config(ColorSpace::default(), BitDepth::Eight)) else {
        return;
    };
    let nits = 10.0;
    let code = ten_bit_code(pq_from_nits(nits));
    let linear = nits / f64::from(rusplay_core::HdrLevels::BT2408_REFERENCE_WHITE_NITS);
    let bt709 = 1.099 * linear.powf(0.45) - 0.099;

    let row = luma_row(
        &mut pipeline,
        &frame(*b"XR30", pq_bt2020(), |_| xrgb2101010(code, code, code)),
    );

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let expected = (bt709 * EIGHT_BIT_MAX).round() as u32;
    assert_codes(&row, expected, 1, "10 cd/m² grey");
}

#[test]
fn a_changed_source_description_reconfigures_the_pipeline() {
    let Some(mut pipeline) = pipeline(config(pq_bt2020(), BitDepth::Ten)) else {
        return;
    };
    let white = pq_from_nits(f64::from(
        rusplay_core::HdrLevels::BT2408_REFERENCE_WHITE_NITS,
    ));

    let first = luma_row(
        &mut pipeline,
        &frame(*b"XR24", srgb(), |_| xrgb8888(255, 255, 255)),
    );
    let second = luma_row(
        &mut pipeline,
        &frame(*b"XR30", pq_bt2020(), |_| xrgb2101010(700, 700, 700)),
    );
    let third = luma_row(
        &mut pipeline,
        &frame(*b"XR24", srgb(), |_| xrgb8888(255, 255, 255)),
    );

    assert_codes(&first, ten_bit_code(white), 1, "sdr white");
    assert_codes(&second, 700, 0, "pq grey");
    assert_eq!(first, third);
}

#[test]
fn downscaling_follows_the_linear_light_setting() {
    let stripes = |column: u32| {
        if column.is_multiple_of(2) {
            xrgb8888(0, 0, 0)
        } else {
            xrgb8888(255, 255, 255)
        }
    };
    let downscaled = |linear_light: bool| {
        let mut config = config(srgb(), BitDepth::Eight);
        config.video.resolution = Some(Resolution::new(SIZE.width / 2, SIZE.height));
        config.scaling.downscale.filter = rusplay_core::ScaleFilter::Bilinear;
        config.scaling.downscale.linear_light = linear_light;
        let mut pipeline = pipeline(config)?;

        Some(luma_row(&mut pipeline, &frame(*b"XR24", srgb(), stripes)))
    };

    let (Some(linear), Some(gamma)) = (downscaled(true), downscaled(false)) else {
        return;
    };
    let srgb_half = 1.055 * 0.5_f64.powf(1.0 / 2.4) - 0.055;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let linear_code = (srgb_half * EIGHT_BIT_MAX).round() as u32;
    assert_codes(&linear[1..linear.len() - 1], linear_code, 1, "linear light");
    assert_codes(&gamma[1..gamma.len() - 1], 128, 1, "gamma light");
}

#[test]
fn dmabuf_captures_convert_like_the_same_shared_memory_frames() {
    let Some(mut pipeline) = dmabuf_pipeline(config(pq_bt2020(), BitDepth::Ten)) else {
        return;
    };
    let gradient = |column: u32| column * 60;

    let shared_srgb = luma_row(
        &mut pipeline,
        &frame(*b"XR24", srgb(), |column| {
            xrgb8888(column * 15, 200, 255 - column * 15)
        }),
    );
    let imported_srgb = luma_row(
        &mut pipeline,
        &dmabuf_frame(*b"XR24", srgb(), |column| {
            xrgb8888(column * 15, 200, 255 - column * 15)
        }),
    );
    let shared_pq = luma_row(
        &mut pipeline,
        &frame(*b"XR30", pq_bt2020(), |column| {
            xrgb2101010(gradient(column), 400, 1023 - gradient(column))
        }),
    );
    let imported_pq = luma_row(
        &mut pipeline,
        &dmabuf_frame(*b"XR30", pq_bt2020(), |column| {
            xrgb2101010(gradient(column), 400, 1023 - gradient(column))
        }),
    );

    assert_eq!(shared_srgb, imported_srgb);
    assert_eq!(shared_pq, imported_pq);
}

#[test]
fn swizzled_dmabuf_formats_read_their_channel_order() {
    let Some(mut pipeline) = dmabuf_pipeline(config(srgb(), BitDepth::Eight)) else {
        return;
    };
    let colour = |column: u32| (column * 15, 255 - column * 15, 90);

    let reference = luma_row(
        &mut pipeline,
        &frame(*b"XR24", srgb(), |column| {
            let (red, green, blue) = colour(column);
            xrgb8888(red, green, blue)
        }),
    );
    let red_first = luma_row(
        &mut pipeline,
        &dmabuf_frame(*b"XB24", srgb(), |column| {
            let (red, green, blue) = colour(column);
            xbgr8888(red, green, blue)
        }),
    );
    let alpha_last = luma_row(
        &mut pipeline,
        &dmabuf_frame(*b"BX24", srgb(), |column| {
            let (red, green, blue) = colour(column);
            bgrx8888(red, green, blue)
        }),
    );

    assert_eq!(reference, red_first);
    for (expected, code) in reference.iter().zip(&alpha_last) {
        assert!(
            expected.abs_diff(*code) <= 1,
            "bx24: code {code}, expected {expected}"
        );
    }
}

#[test]
fn an_unimportable_dmabuf_asks_the_capture_for_shared_memory() {
    let Ok(gpu) = Gpu::open() else {
        return;
    };
    let fallback = ShmFallback::default();
    let mut with_fallback =
        GpuPipeline::new(gpu.clone(), Config::default(), Some(fallback.clone()));
    let mut without = GpuPipeline::new(gpu, Config::default(), None);

    let skipped = with_fallback.encode(&unimportable_frame(), true);
    let failed = without.encode(&unimportable_frame(), true);

    assert!(matches!(skipped, Ok(None)));
    assert!(fallback.is_requested());
    assert!(with_fallback.encoder.is_none());
    assert!(matches!(
        failed,
        Err(HostError::Scale(
            ScaleError::DmaBufFormatUnsupported(_)
                | ScaleError::DmaBufImportUnavailable
                | ScaleError::DmaBufNeedsVulkan
        ))
    ));
}

#[test]
fn gpu_fence_waits_convert_like_cpu_fence_waits() {
    let mut on = config(srgb(), BitDepth::Eight);
    on.capture.dmabuf_gpu_fence_wait = true;
    let mut off = on.clone();
    off.capture.dmabuf_gpu_fence_wait = false;
    let (Some(mut gpu_wait), Some(mut cpu_wait)) = (dmabuf_pipeline(on), dmabuf_pipeline(off))
    else {
        return;
    };
    let texel = |column: u32| xrgb8888(column * 15, 200, 255 - column * 15);

    let gpu_waited = luma_row(&mut gpu_wait, &dmabuf_frame(*b"XR24", srgb(), texel));
    let cpu_waited = luma_row(&mut cpu_wait, &dmabuf_frame(*b"XR24", srgb(), texel));

    assert!(gpu_wait.fence_waiter.is_some());
    assert!(cpu_wait.fence_waiter.is_none());
    assert_eq!(gpu_waited, cpu_waited);
}

#[test]
fn a_fence_the_gpu_cannot_wait_on_falls_back_to_the_cpu_wait() {
    let mut config = config(srgb(), BitDepth::Eight);
    config.capture.dmabuf_fence_timeout_ms = 5;
    let Some(pipeline) = dmabuf_pipeline(config) else {
        return;
    };
    let (unready, _peer) = std::os::unix::net::UnixStream::pair().unwrap();

    let fence = pipeline.gate_on_writes(unready.as_fd()).unwrap();

    assert!(pipeline.fence_waiter.is_some());
    assert!(fence.is_none());
}
