#![cfg(feature = "svt-av1")]

use rusplay_config::{SvtAv1Config, VideoConfig, VideoEncoderConfig};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorSpace, Frame, FrameBuffer, PixelFormat, Resolution,
    Timestamp,
};
use rusplay_encode::input::fourcc_for;

const RESOLUTION: Resolution = Resolution::new(128, 64);
const FORMAT: PixelFormat = PixelFormat {
    chroma: ChromaSubsampling::Yuv420,
    depth: BitDepth::Eight,
};
const FRAME_INTERVAL_MICROS: u64 = 16_667;
const MAX_FRAMES: u64 = 4_000;
const REQUESTED: u64 = 16;

fn timestamp(index: u64) -> Timestamp {
    Timestamp::from_micros(index * FRAME_INTERVAL_MICROS)
}

fn frame(index: u64) -> Frame {
    let width = RESOLUTION.width as usize;
    let height = RESOLUTION.height as usize;
    let shift = usize::try_from(index % 256).unwrap_or_default();
    let luma = (0..height).flat_map(|y| (0..width).map(move |x| x + y + shift));
    let chroma = std::iter::repeat_n(128, width.div_ceil(2) * height.div_ceil(2) * 2);
    let data: Vec<u8> = luma
        .map(|value| u8::try_from(value % 256).unwrap_or_default())
        .chain(chroma)
        .collect();

    Frame {
        resolution: RESOLUTION,
        format: fourcc_for(FORMAT),
        color: ColorSpace::default(),
        timestamp: timestamp(index),
        buffer: FrameBuffer::Shm {
            data: data.into(),
            stride: RESOLUTION.width,
        },
    }
}

#[test]
fn a_random_access_encoder_opens_and_keys_a_requested_frame() {
    let config = VideoConfig {
        codec: Codec::Av1,
        pixel_format: FORMAT,
        resolution: Some(RESOLUTION),
        encoder: VideoEncoderConfig::SvtAv1(SvtAv1Config {
            low_latency: false,
            ..SvtAv1Config::default()
        }),
        ..VideoConfig::default()
    };
    let mut encoder = rusplay_encode::open(&config).unwrap();

    let mut keyframes = Vec::new();
    for index in 0..MAX_FRAMES {
        if index == REQUESTED {
            encoder.request_keyframe();
        }
        let output = encoder.encode(&frame(index)).unwrap();
        if let Some(keyframe) = output.filter(|output| output.keyframe) {
            keyframes.push(keyframe.timestamp);
        }
        if keyframes.contains(&timestamp(REQUESTED)) {
            break;
        }
    }

    assert_eq!(keyframes.first(), Some(&timestamp(0)));
    assert!(keyframes.contains(&timestamp(REQUESTED)), "{keyframes:?}");
}
