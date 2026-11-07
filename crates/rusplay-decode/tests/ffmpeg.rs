#![cfg(feature = "ffmpeg")]

mod common;

use std::{io, num::NonZeroU32, path::PathBuf, process::Command};

use rusplay_config::SoftwareDecoderConfig;
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution, Timestamp,
};
use rusplay_decode::{DecodeError, DecodedBuffer, DecodedFrame, FfmpegDecoder, VideoDecoder};

const FIXTURE_RESOLUTION: Resolution = Resolution::new(256, 144);
const REORDER_DELAY_LIMIT: usize = 8;

fn open(codec: Codec, threads: Option<u32>) -> FfmpegDecoder {
    FfmpegDecoder::new(
        codec,
        &SoftwareDecoderConfig {
            threads: threads.and_then(NonZeroU32::new),
        },
    )
    .unwrap_or_else(|error| panic!("opening the {codec} software decoder: {error}"))
}

fn packets(codec: Codec, name: &str) -> Vec<EncodedFrame> {
    common::annexb_access_units(codec, &common::fixture(name))
}

fn decode_all(codec: Codec, name: &str, threads: Option<u32>) -> Vec<DecodedFrame> {
    let mut decoder = open(codec, threads);
    let frames = packets(codec, name);
    let mut outputs = Vec::new();

    for frame in &frames {
        outputs.extend(decoder.decode(frame).unwrap());
    }
    outputs.extend(decoder.flush().unwrap());
    assert!(decoder.flush().unwrap().is_empty());

    outputs
}

fn cpu(frame: &DecodedFrame) -> &rusplay_decode::CpuFrame {
    match &frame.buffer {
        DecodedBuffer::Cpu(cpu) => cpu,
        DecodedBuffer::DmaBuf(_) => panic!("software decode produced a dma-buf"),
    }
}

fn tight(frame: &DecodedFrame) -> Vec<u8> {
    let mut bytes = Vec::new();

    for plane in cpu(frame).planes() {
        let width = usize::try_from(plane.width).unwrap();
        let sample = usize::from(plane.bytes_per_sample);
        let row = width * sample;

        for y in 0..plane.height {
            let start = usize::try_from(y).unwrap() * plane.stride;
            bytes.extend_from_slice(&plane.data[start..start + row]);
        }
    }

    bytes
}

fn assert_planes(frame: &DecodedFrame, format: PixelFormat) {
    let picture = cpu(frame);

    assert_eq!(frame.resolution, FIXTURE_RESOLUTION);
    assert_eq!(frame.format, format);
    assert_eq!(picture.plane_count(), 3);

    let expected = |index: usize| -> (u32, u32) {
        let Resolution { width, height } = FIXTURE_RESOLUTION;
        if index == 0 {
            return (width, height);
        }
        match format.chroma {
            ChromaSubsampling::Yuv420 => (width / 2, height / 2),
            ChromaSubsampling::Yuv422 => (width / 2, height),
            ChromaSubsampling::Yuv444 => (width, height),
        }
    };
    let bytes_per_sample = if format.depth == BitDepth::Eight {
        1
    } else {
        2
    };

    for index in 0..3 {
        let plane = picture.plane(index).unwrap();
        let (width, height) = expected(index);

        assert_eq!(plane.width, width);
        assert_eq!(plane.height, height);
        assert_eq!(plane.bytes_per_sample, bytes_per_sample);
        assert!(plane.stride >= usize::try_from(width).unwrap() * usize::from(bytes_per_sample));
        assert!(plane.data.iter().any(|byte| *byte != 0) || index != 0);
    }
}

fn assert_one_picture_per_packet(codec: Codec, name: &str, format: PixelFormat) {
    let frames = packets(codec, name);
    let mut decoder = open(codec, None);
    let mut outputs = Vec::new();

    for frame in &frames {
        let output = decoder
            .decode(frame)
            .unwrap()
            .unwrap_or_else(|| panic!("no picture for {:?}", frame.timestamp));

        assert_eq!(output.timestamp, frame.timestamp);
        assert_planes(&output, format);
        outputs.push(output);
    }

    assert!(decoder.flush().unwrap().is_empty());
    assert_eq!(outputs.len(), frames.len());
}

fn assert_reorder(codec: Codec, name: &str, count: usize) {
    let frames = packets(codec, name);
    assert_eq!(frames.len(), count);

    let mut decoder = open(codec, None);
    let mut outputs = Vec::new();
    for frame in &frames {
        outputs.extend(decoder.decode(frame).unwrap());
    }
    let before_flush = outputs.len();
    outputs.extend(decoder.flush().unwrap());

    assert!(before_flush < count);
    assert!(before_flush + REORDER_DELAY_LIMIT >= count);
    assert_eq!(outputs.len(), count);
    assert!(decoder.flush().unwrap().is_empty());

    let mut timestamps: Vec<Timestamp> = outputs.iter().map(|output| output.timestamp).collect();
    timestamps.sort_unstable();
    let mut expected: Vec<Timestamp> = frames.iter().map(|frame| frame.timestamp).collect();
    expected.sort_unstable();
    assert_eq!(timestamps, expected);

    for output in &outputs {
        assert_eq!(output.resolution, FIXTURE_RESOLUTION);
    }
}

fn eight_bit(chroma: ChromaSubsampling) -> PixelFormat {
    PixelFormat {
        chroma,
        depth: BitDepth::Eight,
    }
}

#[test]
fn decodes_h264_one_picture_per_packet() {
    assert_one_picture_per_packet(
        Codec::H264,
        "h264-420-8bit.h264",
        eight_bit(ChromaSubsampling::Yuv420),
    );
}

#[test]
fn decodes_hevc_one_picture_per_packet() {
    assert_one_picture_per_packet(
        Codec::Hevc,
        "hevc-420-8bit.hevc",
        eight_bit(ChromaSubsampling::Yuv420),
    );
}

#[test]
fn decodes_hevc_main10_into_low_ten_bit_samples() {
    let outputs = decode_all(Codec::Hevc, "hevc-420-10bit.hevc", None);
    let format = PixelFormat {
        chroma: ChromaSubsampling::Yuv420,
        depth: BitDepth::Ten,
    };

    assert_eq!(outputs.len(), 8);
    for output in &outputs {
        assert_planes(output, format);
        let plane = cpu(output).plane(0).unwrap();
        let mut saw_sample = false;

        for row in 0..plane.height {
            let start = usize::try_from(row).unwrap() * plane.stride;
            for column in 0..plane.width {
                let offset = start + usize::try_from(column).unwrap() * 2;
                let sample = u16::from_le_bytes(plane.data[offset..offset + 2].try_into().unwrap());

                assert!(sample <= 1_023);
                saw_sample |= sample != 0;
            }
        }
        assert!(saw_sample);
    }
}

#[test]
fn decodes_hevc_422_with_full_height_chroma() {
    assert_one_picture_per_packet(
        Codec::Hevc,
        "hevc-422-8bit.hevc",
        eight_bit(ChromaSubsampling::Yuv422),
    );
}

#[test]
fn h264_with_b_frames_holds_pictures_and_flushes_them() {
    assert_reorder(Codec::H264, "h264-420-8bit-bframes.h264", 12);
}

#[test]
fn hevc_with_b_frames_holds_pictures_and_flushes_them() {
    assert_reorder(Codec::Hevc, "hevc-420-8bit-bframes.hevc", 12);
}

#[test]
fn mbaff_frames_decode_at_the_fixture_size() {
    let outputs = decode_all(Codec::H264, "h264-420-8bit-mbaff.h264", None);

    assert_eq!(outputs.len(), 12);
    for output in &outputs {
        assert_planes(output, eight_bit(ChromaSubsampling::Yuv420));
    }
}

#[test]
fn rejects_frames_of_another_codec() {
    let mut decoder = open(Codec::H264, None);
    let frame = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit.ivf")).remove(0);

    assert!(matches!(
        decoder.decode(&frame),
        Err(DecodeError::CodecMismatch {
            expected: Codec::H264,
            found: Codec::Av1,
        })
    ));
}

#[test]
fn a_hevc_frame_that_lost_its_reference_fails_and_the_next_keyframe_decodes() {
    let mut decoder = open(Codec::Hevc, Some(1));
    let frames = packets(Codec::Hevc, "hevc-420-8bit.hevc");
    assert!(
        frames.len() >= 3,
        "the fixture needs a frame after the lost one"
    );

    assert!(decoder.decode(&frames[0]).unwrap().is_some());
    let lost = decoder.decode(&frames[2]);

    assert!(
        matches!(lost, Err(DecodeError::Ffmpeg { .. })),
        "a missing reference was {lost:?}"
    );
    assert!(decoder.decode(&frames[0]).unwrap().is_some());
}

#[test]
fn an_empty_packet_produces_nothing() {
    let mut decoder = open(Codec::H264, Some(1));
    let mut frame = packets(Codec::H264, "h264-420-8bit.h264").remove(0);
    frame.data = Vec::new().into();

    assert!(decoder.decode(&frame).unwrap().is_none());
}

#[test]
fn one_thread_and_two_threads_decode_the_same_pictures() {
    let single = decode_all(Codec::H264, "h264-420-8bit.h264", Some(1));
    let sliced = decode_all(Codec::H264, "h264-420-8bit.h264", Some(2));

    assert_eq!(single.len(), sliced.len());
    for (left, right) in single.iter().zip(&sliced) {
        assert_eq!(tight(left), tight(right));
    }
}

#[test]
fn pictures_match_ffmpeg_rawvideo() {
    let cases = [
        (
            Codec::H264,
            "h264-420-8bit.h264",
            "yuv420p",
            eight_bit(ChromaSubsampling::Yuv420),
        ),
        (
            Codec::Hevc,
            "hevc-420-8bit.hevc",
            "yuv420p",
            eight_bit(ChromaSubsampling::Yuv420),
        ),
        (
            Codec::Hevc,
            "hevc-420-10bit.hevc",
            "yuv420p10le",
            PixelFormat {
                chroma: ChromaSubsampling::Yuv420,
                depth: BitDepth::Ten,
            },
        ),
        (
            Codec::Hevc,
            "hevc-422-8bit.hevc",
            "yuv422p",
            eight_bit(ChromaSubsampling::Yuv422),
        ),
    ];

    for (codec, name, pix_fmt, format) in cases {
        let Some(reference) = ffmpeg_raw(name, pix_fmt) else {
            eprintln!("skipped ffmpeg pixel compare: ffmpeg binary missing");
            return;
        };
        let decoded: Vec<u8> = decode_all(codec, name, None)
            .iter()
            .inspect(|frame| assert_planes(frame, format))
            .flat_map(tight)
            .collect();

        assert_eq!(decoded, reference, "{name}");
    }
}

fn ffmpeg_raw(name: &str, pix_fmt: &str) -> Option<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    let output = match Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => panic!("running ffmpeg: {error}"),
    };

    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(output.stdout)
}

const MOTION_FIXTURE: &str = "hevc-rusplay-motion-8bit";
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn motion_packets() -> Vec<(EncodedFrame, [u64; 3])> {
    let encoded = common::fixture(&format!("{MOTION_FIXTURE}.hevc"));
    let index = common::fixture(&format!("{MOTION_FIXTURE}.index"));
    let mut offset = 0;

    std::str::from_utf8(&index)
        .unwrap()
        .lines()
        .enumerate()
        .map(|(number, line)| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let size: usize = fields[0].parse().unwrap();
            let checksums = [1, 2, 3].map(|field| u64::from_str_radix(fields[field], 16).unwrap());
            let packet = EncodedFrame {
                codec: Codec::Hevc,
                timestamp: Timestamp::from_micros(u64::try_from(number).unwrap() * 16_666),
                keyframe: number == 0,
                data: encoded[offset..offset + size].to_vec().into(),
            };
            offset += size;
            (packet, checksums)
        })
        .collect()
}

fn checksums(frame: &DecodedFrame) -> [u64; 3] {
    let cpu = cpu(frame);

    [0, 1, 2].map(|plane| {
        let pixels = cpu.plane(plane).unwrap();
        let row_bytes = pixels.width as usize * usize::from(pixels.bytes_per_sample);
        pixels
            .data
            .chunks_exact(pixels.stride)
            .take(pixels.height as usize)
            .flat_map(|row| row[..row_bytes].iter().copied())
            .fold(FNV_OFFSET, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
            })
    })
}

#[test]
fn a_rejected_packet_keeps_the_references_the_next_frame_predicts_from() {
    let mut decoder = open(Codec::Hevc, Some(1));
    let packets = motion_packets();
    let (resumed, expected) = &packets[10];
    let broken = EncodedFrame {
        data: [
            0, 0, 1, 0x02, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ]
        .to_vec()
        .into(),
        ..resumed.clone()
    };

    for (packet, _) in &packets[..10] {
        assert!(decoder.decode(packet).unwrap().is_some());
    }
    let rejected = decoder.decode(&broken);
    let picture = decoder.decode(resumed).unwrap().unwrap();

    assert!(
        matches!(rejected, Err(DecodeError::Ffmpeg { .. })),
        "a broken slice was {rejected:?}"
    );
    assert_eq!(checksums(&picture), *expected);
}
