#![cfg(feature = "vaapi")]

mod common;
#[path = "../src/hardware.rs"]
mod hardware;

use std::{path::PathBuf, process::Command};

use rusplay_config::VaapiDecoderConfig;
use rusplay_core::{BitDepth, Codec, EncodedFrame, Resolution, Timestamp};
use rusplay_decode::{DecodeError, DecodedBuffer, VaapiDecoder, VideoDecoder};

const SIZE: Resolution = Resolution::new(1280, 720);
const CASES: [(&str, BitDepth); 3] = [
    ("hevc-rusplay-motion-8bit", BitDepth::Eight),
    ("hevc-rusplay-motion-10bit", BitDepth::Ten),
    ("hevc-rusplay-recovery-10bit", BitDepth::Ten),
];
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

type Record = (usize, [u64; 3]);

fn records(name: &str) -> Vec<Record> {
    let bytes = common::fixture(&format!("{name}.index"));
    let text = std::str::from_utf8(&bytes).unwrap();

    text.lines()
        .map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 4);
            (
                fields[0].parse().unwrap(),
                [1, 2, 3].map(|index| u64::from_str_radix(fields[index], 16).unwrap()),
            )
        })
        .collect()
}

fn hash(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

#[test]
fn rusplay_hevc_matches_software_reference() {
    let config = VaapiDecoderConfig {
        copy_output: true,
        ..VaapiDecoderConfig::default()
    };
    if !hardware::device_present(&config.device) {
        return;
    }

    for (name, depth) in CASES {
        let encoded = common::fixture(&format!("{name}.hevc"));
        let records = records(name);
        assert_eq!(
            records.iter().map(|(length, _)| length).sum::<usize>(),
            encoded.len()
        );
        let mut decoder = match VaapiDecoder::new(Codec::Hevc, &config) {
            Ok(decoder) => decoder,
            Err(error @ DecodeError::CodecUnsupported { .. }) => {
                hardware::unsupported(error);
                return;
            }
            Err(error) => panic!("opening HEVC: {error}"),
        };
        let mut offset = 0;
        for (index, (size, expected)) in records.iter().enumerate() {
            let packet = EncodedFrame {
                codec: Codec::Hevc,
                timestamp: Timestamp::from_micros(u64::try_from(index).unwrap() * 16_666),
                keyframe: index == 0,
                data: encoded[offset..offset + size].to_vec().into(),
            };
            offset += size;
            let output = match decoder.decode(&packet) {
                Ok(Some(frame)) => frame,
                Err(
                    error @ (DecodeError::BitDepthUnsupported { .. }
                    | DecodeError::ChromaUnsupported { .. }),
                ) => {
                    hardware::unsupported(error);
                    break;
                }
                other => panic!("{name} frame {index}: {other:?}"),
            };
            let DecodedBuffer::Cpu(cpu) = &output.buffer else {
                panic!("expected CPU output")
            };

            assert_eq!(output.resolution, SIZE);
            assert_eq!(output.format.depth, depth);
            assert_eq!(cpu.plane_count(), expected.len());
            for (plane, expected) in expected.iter().enumerate() {
                let pixels = cpu.plane(plane).unwrap();
                let row_bytes = pixels.width as usize * usize::from(pixels.bytes_per_sample);
                let actual = hash(
                    pixels
                        .data
                        .chunks_exact(pixels.stride)
                        .take(pixels.height as usize)
                        .flat_map(|row| row[..row_bytes].iter().copied()),
                );

                assert_eq!(
                    actual, *expected,
                    "rusplay {name} frame {index} plane {plane}: expected {expected:016x}, got {actual:016x}"
                );
            }
        }
        if offset == encoded.len() {
            eprintln!(
                "rusplay {name}: {} frames match the software reference",
                records.len()
            );
        }
        assert!(decoder.flush().unwrap().is_empty());
    }
}

#[test]
fn ffmpeg_vaapi_hevc_matches_software_reference() {
    let config = VaapiDecoderConfig::default();
    if !hardware::device_present(&config.device) {
        return;
    }
    let available = Command::new("ffmpeg")
        .args(["-hide_banner", "-hwaccels"])
        .output();
    if !available.is_ok_and(|output| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "vaapi")
    }) {
        hardware::unsupported("ffmpeg with VAAPI hardware decoding");
        return;
    }

    for (name, depth) in CASES {
        let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(format!("{name}.hevc"));
        let (download, planar, sample) = if depth == BitDepth::Eight {
            ("hwdownload,format=nv12", "yuv420p", 1)
        } else {
            ("hwdownload,format=p010le", "yuv420p10le", 2)
        };
        let output = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-hwaccel",
                "vaapi",
                "-hwaccel_output_format",
                "vaapi",
                "-hwaccel_device",
            ])
            .arg(&config.device)
            .arg("-i")
            .arg(input)
            .args([
                "-vf",
                download,
                "-fps_mode",
                "passthrough",
                "-pix_fmt",
                planar,
                "-f",
                "rawvideo",
                "pipe:1",
            ])
            .output()
            .unwrap();
        let expected = records(name);
        let luma = usize::try_from(SIZE.pixel_count()).unwrap() * sample;
        let frame_bytes = luma * 3 / 2;

        assert!(
            output.status.success(),
            "ffmpeg {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "ffmpeg {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout.len(),
            expected.len() * frame_bytes,
            "ffmpeg {name}"
        );
        for (index, (frame, (_, checksums))) in output
            .stdout
            .chunks_exact(frame_bytes)
            .zip(&expected)
            .enumerate()
        {
            let planes = [
                &frame[..luma],
                &frame[luma..luma * 5 / 4],
                &frame[luma * 5 / 4..],
            ];
            for (plane, (pixels, expected)) in planes.into_iter().zip(checksums).enumerate() {
                let actual = hash(pixels.iter().copied());

                assert_eq!(
                    actual, *expected,
                    "ffmpeg {name} frame {index} plane {plane}: expected {expected:016x}, got {actual:016x}"
                );
            }
        }
        eprintln!(
            "ffmpeg {name}: {} frames match the software reference",
            expected.len()
        );
    }
}
