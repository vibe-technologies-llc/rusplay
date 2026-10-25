#![cfg(feature = "software")]

mod common;

use rusplay_config::SoftwareDecoderConfig;
use rusplay_core::{BitDepth, ChromaSubsampling, Codec, PixelFormat, Resolution};
use rusplay_decode::{DecodeError, DecodedBuffer, Rav1dDecoder, VideoDecoder};

const FIXTURE_RESOLUTION: Resolution = Resolution::new(256, 144);
const FIXTURE_FRAMES: usize = 8;

fn decode_every_packet(fixture: &str, expected: PixelFormat) {
    let frames = common::ivf_frames(Codec::Av1, &common::fixture(fixture));
    let mut decoder = Rav1dDecoder::new(&SoftwareDecoderConfig::default()).unwrap();

    assert_eq!(frames.len(), FIXTURE_FRAMES);

    for frame in &frames {
        let output = decoder
            .decode(frame)
            .unwrap()
            .unwrap_or_else(|| panic!("no picture for the packet at {:?}", frame.timestamp));

        assert_eq!(output.resolution, FIXTURE_RESOLUTION);
        assert_eq!(output.format, expected);
        assert_eq!(output.timestamp, frame.timestamp);

        let DecodedBuffer::Cpu(picture) = &output.buffer else {
            panic!("rav1d produced a non-CPU buffer: {:?}", output.buffer);
        };
        let planes: Vec<_> = picture.planes().collect();
        let bytes_per_sample = if expected.depth == BitDepth::Eight {
            1
        } else {
            2
        };

        assert_eq!(planes.len(), 3);
        assert_eq!(planes[0].width, FIXTURE_RESOLUTION.width);
        assert_eq!(planes[0].height, FIXTURE_RESOLUTION.height);
        for plane in &planes {
            assert_eq!(plane.bytes_per_sample, bytes_per_sample);
            assert!(plane.stride >= plane.width as usize * usize::from(bytes_per_sample));
            assert!(plane.data.len() >= plane.stride * plane.height as usize);
        }
    }
}

#[test]
fn decodes_8_bit_420_one_picture_per_packet() {
    decode_every_packet(
        "av1-420-8bit.ivf",
        PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth: BitDepth::Eight,
        },
    );
}

#[test]
fn decodes_10_bit_420_one_picture_per_packet() {
    decode_every_packet(
        "av1-420-10bit.ivf",
        PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth: BitDepth::Ten,
        },
    );
}

#[test]
fn decodes_8_bit_444_with_full_chroma_planes() {
    decode_every_packet(
        "av1-444-8bit.ivf",
        PixelFormat {
            chroma: ChromaSubsampling::Yuv444,
            depth: BitDepth::Eight,
        },
    );
}

#[test]
fn rejects_frames_of_another_codec() {
    let mut decoder = Rav1dDecoder::new(&SoftwareDecoderConfig::default()).unwrap();
    let frame =
        common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit.h264")).remove(0);

    assert!(matches!(
        decoder.decode(&frame),
        Err(DecodeError::CodecMismatch {
            expected: Codec::Av1,
            found: Codec::H264,
        })
    ));
}
