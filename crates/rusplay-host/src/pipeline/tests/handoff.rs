use std::{
    num::NonZeroU32,
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

use rusplay_capture::{BufferLease, DRM_FORMAT_MOD_LINEAR, DrmFourcc};
use rusplay_config::{SoftwareDecoderConfig, VaapiConfig, VideoEncoderConfig};
use rusplay_core::{BitDepth, ChromaSubsampling, Codec};
use rusplay_decode::{DecodedBuffer, Rav1dDecoder, VideoDecoder};
use rusplay_scale::udmabuf;

use super::*;

const FRAMES: u32 = 12;
const BITRATE_KBPS: u32 = 8_000;
const BENCH_RESOLUTION: Resolution = Resolution::new(3840, 2160);
const BENCH_WARMUP_FRAMES: u32 = 30;
const BENCH_FRAMES: u32 = 240;
const BENCH_SOURCES: u32 = 3;

fn vaapi_config(codec: Codec, depth: BitDepth, handoff: bool) -> Config {
    let mut config = Config::default();
    config.video.codec = codec;
    config.video.pixel_format = PixelFormat {
        chroma: ChromaSubsampling::Yuv420,
        depth,
    };
    config.video.color = ColorSpace {
        transfer: TransferFunction::Bt709,
        ..ColorSpace::default()
    };
    config.video.bitrate_kbps = BITRATE_KBPS;
    config.video.encoder = VideoEncoderConfig::Vaapi(VaapiConfig::default());
    config.video.encoder_gpu_handoff = handoff;
    config
}

fn supported(config: &Config) -> bool {
    rusplay_encode::capabilities(&config.video.encoder)
        .is_ok_and(|caps| caps.supports(config.video.codec, config.video.pixel_format))
}

fn pattern(resolution: Resolution, index: u32, stride: u32) -> Vec<u8> {
    let row_padding = (stride - resolution.width * 4) as usize;

    (0..resolution.height)
        .flat_map(|y| {
            (0..resolution.width)
                .map(move |x| {
                    let shifted = x + 3 * index;
                    let red = (shifted * 255 / resolution.width.max(1)) % 256;
                    let green = (y * 255 / resolution.height.max(1) + index * 5) % 256;
                    let blue = (((shifted / 16) + (y / 16)) % 2) * 160 + 40;
                    (0xff << 24) | (red << 16) | (green << 8) | blue
                })
                .flat_map(u32::to_le_bytes)
                .chain(std::iter::repeat_n(0, row_padding))
                .collect::<Vec<u8>>()
        })
        .collect()
}

fn shm_frame(resolution: Resolution, index: u32) -> Frame {
    let stride = resolution.width * 4;

    Frame {
        resolution,
        format: DrmFourcc::from_code(*b"XR24"),
        color: ColorSpace {
            transfer: TransferFunction::Srgb,
            ..ColorSpace::default()
        },
        timestamp: Timestamp::from_micros(u64::from(index) * 16_667),
        buffer: FrameBuffer::Shm {
            data: pattern(resolution, index, stride),
            stride,
        },
    }
}

fn encode_stream(config: Config, resolution: Resolution) -> Option<(Vec<EncodedFrame>, bool)> {
    let mut pipeline = pipeline(config)?;
    let handed_off = pipeline.hands_off();

    let encoded = (0..FRAMES)
        .filter_map(|index| {
            pipeline
                .encode(&shm_frame(resolution, index), index == 0)
                .unwrap()
        })
        .collect();

    Some((encoded, handed_off && pipeline.hands_off()))
}

fn decode_stream(encoded: &[EncodedFrame]) -> Vec<Vec<Vec<u8>>> {
    let mut decoder = Rav1dDecoder::new(&SoftwareDecoderConfig {
        threads: NonZeroU32::new(1),
    })
    .unwrap();

    encoded
        .iter()
        .filter_map(|frame| decoder.decode(frame).unwrap())
        .map(|decoded| {
            let DecodedBuffer::Cpu(cpu) = decoded.buffer else {
                unreachable!("rav1d decodes to memory");
            };
            cpu.planes()
                .map(|plane| {
                    let row_bytes = plane.width as usize * usize::from(plane.bytes_per_sample);
                    (0..plane.height as usize)
                        .flat_map(|row| &plane.data[row * plane.stride..][..row_bytes])
                        .copied()
                        .collect()
                })
                .collect()
        })
        .collect()
}

#[test]
fn handed_off_frames_decode_like_read_back_frames() {
    for depth in [BitDepth::Eight, BitDepth::Ten] {
        for resolution in [Resolution::new(640, 360), Resolution::new(402, 226)] {
            let config = vaapi_config(Codec::Av1, depth, true);
            if !supported(&config) {
                continue;
            }
            let Some((handed_off, used_handoff)) = encode_stream(config, resolution) else {
                return;
            };
            let (read_back, used_readback) =
                encode_stream(vaapi_config(Codec::Av1, depth, false), resolution).unwrap();

            let from_handoff = decode_stream(&handed_off);
            let from_readback = decode_stream(&read_back);

            assert!(used_handoff, "{depth:?} {resolution}: the dma-buf path ran");
            assert!(!used_readback);
            assert_eq!(handed_off.len(), FRAMES as usize);
            assert!(handed_off[0].keyframe);
            assert_eq!(from_handoff.len(), FRAMES as usize);
            assert!(
                from_handoff == from_readback,
                "{depth:?} {resolution}: the handed-off stream decodes to other samples"
            );
        }
    }
}

#[test]
fn a_refused_handoff_falls_back_to_the_readback() {
    let mut config = vaapi_config(Codec::Av1, BitDepth::Eight, true);
    if !supported(&config) {
        return;
    }
    if let VideoEncoderConfig::Vaapi(vaapi) = &mut config.video.encoder {
        vaapi.device = "/dev/null".into();
    }
    let Some(mut pipeline) = pipeline(config) else {
        return;
    };
    let handed_off = pipeline.hands_off();

    let refused = pipeline.encode(&shm_frame(Resolution::new(640, 360), 0), true);

    assert!(handed_off);
    assert!(!pipeline.hands_off());
    assert!(matches!(refused, Err(HostError::Encode(_))));
}

fn dmabuf_source(resolution: Resolution, index: u32) -> Option<OwnedFd> {
    let stride = (resolution.width * 4).next_multiple_of(256);
    udmabuf::create(&pattern(resolution, index, stride))
}

fn dmabuf_capture(fd: &OwnedFd, resolution: Resolution, index: u32) -> Frame {
    Frame {
        resolution,
        format: DrmFourcc::from_code(*b"XR24"),
        color: ColorSpace {
            transfer: TransferFunction::Srgb,
            ..ColorSpace::default()
        },
        timestamp: Timestamp::from_micros(u64::from(index) * 16_667),
        buffer: FrameBuffer::DmaBuf {
            fd: fd.try_clone().unwrap(),
            modifier: DRM_FORMAT_MOD_LINEAR,
            planes: vec![Plane {
                offset: 0,
                stride: (resolution.width * 4).next_multiple_of(256),
            }],
            lease: BufferLease::detached(),
        },
    }
}

fn median(times: &mut [Duration]) -> Duration {
    times.sort_unstable();
    times[times.len() / 2]
}

#[test]
#[ignore = "measures 4K encode latency on the VAAPI device"]
fn handoff_frame_times() {
    for (codec, depth) in [
        (Codec::Av1, BitDepth::Eight),
        (Codec::Av1, BitDepth::Ten),
        (Codec::Hevc, BitDepth::Eight),
        (Codec::Hevc, BitDepth::Ten),
    ] {
        let config = vaapi_config(codec, depth, true);
        if !supported(&config) {
            continue;
        }
        let (Some(mut handoff), Some(mut readback)) = (
            dmabuf_pipeline(config),
            dmabuf_pipeline(vaapi_config(codec, depth, false)),
        ) else {
            return;
        };
        let sources: Vec<OwnedFd> = (0..BENCH_SOURCES)
            .filter_map(|index| dmabuf_source(BENCH_RESOLUTION, index))
            .collect();
        let mut handoff_times = Vec::new();
        let mut readback_times = Vec::new();

        for index in 0..BENCH_WARMUP_FRAMES + BENCH_FRAMES {
            let fd = &sources[(index % BENCH_SOURCES) as usize];
            let mut order = [
                (&mut handoff, &mut handoff_times),
                (&mut readback, &mut readback_times),
            ];
            if index % 2 == 1 {
                order.reverse();
            }
            for (pipeline, times) in order {
                let frame = dmabuf_capture(fd, BENCH_RESOLUTION, index);
                let started = Instant::now();
                pipeline.encode(&frame, index == 0).unwrap();
                if index >= BENCH_WARMUP_FRAMES {
                    times.push(started.elapsed());
                }
            }
        }

        assert!(handoff.hands_off());
        assert!(!readback.hands_off());
        println!(
            "{codec:?} {depth:?} {BENCH_RESOLUTION}: handoff {:.2} ms, readback {:.2} ms per frame (median of {BENCH_FRAMES})",
            median(&mut handoff_times).as_secs_f64() * 1000.0,
            median(&mut readback_times).as_secs_f64() * 1000.0,
        );
    }
}
