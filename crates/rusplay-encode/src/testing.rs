use std::{fs::File, os::fd::OwnedFd};

use rusplay_capture::{DrmFourcc, Frame, FrameBuffer};
use rusplay_config::{EncoderParams, VideoConfig, VideoEncoderConfig};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorSpace, PixelFormat, Resolution, Timestamp,
};

use crate::{
    EncodeError, VideoEncoder,
    hdr::fixtures::{hdr10_color, signalled},
    input::{fixtures::yuv_frame, fourcc_for},
    open,
};

pub const FRAME_INTERVAL_MICROS: u64 = 16_666;
pub const TEST_RESOLUTION: Resolution = Resolution::new(128, 64);

pub fn pixel(chroma: ChromaSubsampling, depth: BitDepth) -> PixelFormat {
    PixelFormat { chroma, depth }
}

pub fn video(codec: Codec, format: PixelFormat, encoder: VideoEncoderConfig) -> VideoConfig {
    VideoConfig {
        resolution: Some(TEST_RESOLUTION),
        codec,
        pixel_format: format,
        bitrate_kbps: 8_000,
        encoder,
        ..VideoConfig::default()
    }
}

pub fn frame(format: PixelFormat, index: u64) -> Frame {
    yuv_frame(
        format,
        TEST_RESOLUTION,
        index,
        Timestamp::from_micros(index * FRAME_INTERVAL_MICROS),
    )
}

pub fn params(pairs: &[(&str, &str)]) -> EncoderParams {
    pairs
        .iter()
        .map(|&(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

pub fn start_code_offsets(data: &[u8]) -> Vec<usize> {
    data.windows(3)
        .enumerate()
        .filter(|(_, window)| *window == [0, 0, 1])
        .map(|(offset, _)| offset + 3)
        .collect()
}

pub fn dmabuf_frame() -> Frame {
    let fd = OwnedFd::from(File::open("/dev/null").unwrap());

    Frame {
        resolution: TEST_RESOLUTION,
        format: DrmFourcc::from_code(*b"XR24"),
        timestamp: Timestamp::ZERO,
        color: ColorSpace::default(),
        buffer: FrameBuffer::DmaBuf {
            fd,
            modifier: 0,
            planes: Vec::new(),
        },
    }
}

pub fn check_stream_starts_on_a_keyframe(
    config: &VideoConfig,
    contains_idr: impl Fn(&[u8]) -> bool,
) {
    let format = config.pixel_format;
    let mut encoder = open(config).unwrap();

    let first = encoder.encode(&frame(format, 0)).unwrap().unwrap();
    assert!(first.keyframe);
    assert!(contains_idr(&first.data));
    assert_eq!(first.codec, config.codec);
    assert_eq!(first.timestamp, Timestamp::ZERO);

    for index in 1..5 {
        let next = encoder.encode(&frame(format, index)).unwrap().unwrap();
        assert!(!next.keyframe);
        assert!(!contains_idr(&next.data));
        assert_eq!(next.timestamp.as_micros(), index * FRAME_INTERVAL_MICROS);
    }
}

pub fn check_keyframe_on_request(config: &VideoConfig, contains_idr: impl Fn(&[u8]) -> bool) {
    let format = config.pixel_format;
    let mut encoder = open(config).unwrap();

    for index in 0..3 {
        encoder.encode(&frame(format, index)).unwrap();
    }
    encoder.request_keyframe();

    let forced = encoder.encode(&frame(format, 3)).unwrap().unwrap();
    let after = encoder.encode(&frame(format, 4)).unwrap().unwrap();

    assert!(forced.keyframe);
    assert!(contains_idr(&forced.data));
    assert!(!after.keyframe);
}

pub fn check_set_bitrate_in_place(config: &VideoConfig) {
    let format = config.pixel_format;
    let mut encoder = open(config).unwrap();
    encoder.encode(&frame(format, 0)).unwrap();

    encoder.set_bitrate(2_000).unwrap();
    let next = encoder.encode(&frame(format, 1)).unwrap();

    assert!(next.is_some());
    assert!(matches!(
        encoder.set_bitrate(0),
        Err(EncodeError::BitrateRejected { kbps: 0, .. })
    ));
}

pub fn check_unknown_parameter_names_the_key(config: &VideoConfig) {
    let error = open(config).err().unwrap();

    assert!(matches!(
        error,
        EncodeError::UnknownParameter { ref key, .. } if key == "no-such-knob"
    ));
}

pub fn check_invalid_value_names_the_key(config: &VideoConfig, key: &str, value: &str) {
    let error = open(config).err().unwrap();

    assert!(matches!(
        error,
        EncodeError::InvalidParameterValue { key: ref got_key, value: ref got_value, .. }
            if got_key == key && got_value == value
    ));
}

pub fn check_wrong_inputs_are_rejected(config: &VideoConfig) {
    let wanted = config.pixel_format;
    let other = pixel(
        ChromaSubsampling::Yuv444,
        if wanted.depth == BitDepth::Eight {
            BitDepth::Ten
        } else {
            BitDepth::Eight
        },
    );
    let mut encoder: Box<dyn VideoEncoder> = open(config).unwrap();

    let wrong_format = encoder.encode(&frame(other, 0)).unwrap_err();
    let dmabuf = encoder.encode(&dmabuf_frame()).unwrap_err();
    let small = yuv_frame(wanted, Resolution::new(64, 64), 0, Timestamp::ZERO);
    let wrong_size = encoder.encode(&small).unwrap_err();

    assert!(matches!(
        wrong_format,
        EncodeError::FrameFormatMismatch { expected, .. } if expected == fourcc_for(wanted)
    ));
    assert!(matches!(dmabuf, EncodeError::CpuFrameRequired { .. }));
    assert!(matches!(
        wrong_size,
        EncodeError::FrameResolutionMismatch { .. }
    ));
}

pub fn check_unspecified_resolution_follows_the_first_frame(config: &VideoConfig) {
    let config = VideoConfig {
        resolution: None,
        ..config.clone()
    };
    let mut encoder = open(&config).unwrap();

    let first = encoder
        .encode(&frame(config.pixel_format, 0))
        .unwrap()
        .unwrap();

    assert!(first.keyframe);
}

pub fn check_colour_reaches_the_decoder(
    config: &VideoConfig,
    decoder: &mut dyn rusplay_decode::VideoDecoder,
) {
    let config = VideoConfig {
        color: hdr10_color(),
        ..config.clone()
    };
    let expected = signalled(&config.color);
    let mut encoder = open(&config).unwrap();

    for index in 0..3 {
        let packet = encoder
            .encode(&frame(config.pixel_format, index))
            .unwrap()
            .unwrap();
        let picture = decoder.decode(&packet).unwrap().unwrap();

        assert_eq!(picture.color, expected, "frame {index}");
    }
}
