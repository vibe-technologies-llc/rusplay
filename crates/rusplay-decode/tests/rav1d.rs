#![cfg(feature = "software")]

mod common;

use std::ops::Range;

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

#[test]
fn decodes_hidden_frames_tiles_and_shown_existing_frames() {
    let frames = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit-altref.ivf"));
    let mut decoder = Rav1dDecoder::new(&SoftwareDecoderConfig::default()).unwrap();

    for frame in &frames {
        let output = decoder
            .decode(frame)
            .unwrap()
            .unwrap_or_else(|| panic!("no picture for the packet at {:?}", frame.timestamp));

        assert_eq!(output.resolution, FIXTURE_RESOLUTION);
    }
}

const OBU_FRAME: u8 = 6;

struct Obu {
    start: usize,
    header_len: usize,
    payload: Range<usize>,
    kind: u8,
}

fn obus(packet: &[u8]) -> Vec<Obu> {
    let mut found = Vec::new();
    let mut cursor = 0;

    while cursor < packet.len() {
        let header = packet[cursor];
        let header_len = 1 + usize::from(header & 0b100 != 0);
        let mut size = 0usize;
        let mut position = cursor + header_len;

        for shift in (0..).step_by(7) {
            let byte = packet[position];
            position += 1;
            size |= usize::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
        }

        found.push(Obu {
            start: cursor,
            header_len,
            payload: position..position + size,
            kind: (header >> 3) & 0xf,
        });
        cursor = position + size;
    }

    found
}

fn leb128(mut value: usize) -> Vec<u8> {
    let mut bytes = Vec::new();

    loop {
        let low = u8::try_from(value & 0x7f).unwrap();
        value >>= 7;
        if value == 0 {
            bytes.push(low);
            return bytes;
        }
        bytes.push(low | 0x80);
    }
}

fn with_tile_data_cut_short(packet: &[u8], cut: usize) -> Vec<u8> {
    let mut rebuilt = Vec::new();

    for obu in obus(packet) {
        if obu.kind == OBU_FRAME {
            let kept = obu.payload.start..obu.payload.end - cut;
            rebuilt.extend_from_slice(&packet[obu.start..obu.start + obu.header_len]);
            rebuilt.extend(leb128(kept.len()));
            rebuilt.extend_from_slice(&packet[kept]);
        } else {
            rebuilt.extend_from_slice(&packet[obu.start..obu.payload.end]);
        }
    }

    rebuilt
}

#[test]
fn tile_data_cut_short_is_an_error_and_the_decoder_decodes_on() {
    let frames = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit.ivf"));

    for broken_index in 0..frames.len() {
        for cut in [1, 8, 64] {
            let mut decoder = Rav1dDecoder::new(&SoftwareDecoderConfig::default()).unwrap();
            for frame in &frames[..broken_index] {
                decoder.decode(frame).unwrap();
            }

            let mut broken = frames[broken_index].clone();
            broken.data = with_tile_data_cut_short(&frames[broken_index].data, cut).into();

            assert!(
                matches!(decoder.decode(&broken), Err(DecodeError::Rav1dSendData(_))),
                "frame {broken_index} cut by {cut}"
            );

            for frame in &frames {
                assert!(decoder.decode(frame).unwrap().is_some());
            }
        }
    }
}
