use std::{
    io::Write,
    process::{Command, Stdio},
    thread,
};

use rusplay_config::RateControl;

use super::*;

const SIZE: Resolution = Resolution::new(1280, 720);
const COUNT: u32 = 64;

fn config(depth: BitDepth, handoff: bool) -> Config {
    let mut config = vaapi_config(Codec::Hevc, depth, handoff);
    config.video.encoder = VideoEncoderConfig::Vaapi(VaapiConfig {
        low_power: false,
        rate_control: RateControl::ConstantQuality { qp: 20 },
        ..VaapiConfig::default()
    });
    config
}

fn stream(config: Config) -> Option<(Vec<EncodedFrame>, bool)> {
    let mut pipeline = pipeline(config)?;
    let mut packets = Vec::new();
    for index in 0..COUNT {
        packets.extend(
            pipeline
                .encode(&shm_frame(SIZE, index), index == 0)
                .unwrap(),
        );
    }

    Some((packets, pipeline.hands_off()))
}

fn decode(depth: BitDepth, packets: &[EncodedFrame]) -> Option<Vec<u8>> {
    let format = if depth == BitDepth::Eight {
        "yuv420p"
    } else {
        "yuv420p10le"
    };
    let child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-hwaccel",
            "none",
            "-threads",
            "1",
            "-f",
            "hevc",
            "-i",
            "pipe:0",
        ])
        .args([
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            format,
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            hardware::unsupported(format_args!("ffmpeg: {error}"));
            return None;
        }
    };
    let bytes: Vec<u8> = packets
        .iter()
        .flat_map(|packet| packet.data.iter().copied())
        .collect();
    let mut input = child.stdin.take().unwrap();
    let writer = thread::spawn(move || input.write_all(&bytes));
    let result = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();

    assert!(
        result.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stderr.is_empty(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Some(result.stdout)
}

#[test]
fn hevc_handoff_and_readback_match_in_luma_and_chroma() {
    for depth in [BitDepth::Eight, BitDepth::Ten] {
        if !supported(&config(depth, true)) {
            continue;
        }
        let Some((packets, handed_off)) = stream(config(depth, true)) else {
            return;
        };
        let Some((readback_packets, read_back)) = stream(config(depth, false)) else {
            return;
        };
        let Some(actual) = decode(depth, &packets) else {
            return;
        };
        let Some(expected) = decode(depth, &readback_packets) else {
            return;
        };
        let sample = if depth == BitDepth::Eight { 1 } else { 2 };
        let frame_bytes = usize::try_from(SIZE.pixel_count()).unwrap() * sample * 3 / 2;

        assert!(handed_off);
        assert!(!read_back);
        assert_eq!(packets.len(), COUNT as usize);
        assert_eq!(actual.len(), frame_bytes * COUNT as usize);
        assert_eq!(actual.len(), expected.len());
        for (index, (actual, expected)) in actual
            .chunks_exact(frame_bytes)
            .zip(expected.chunks_exact(frame_bytes))
            .enumerate()
        {
            let differences = actual.iter().zip(expected).filter(|(a, b)| a != b).count();

            assert_eq!(
                differences, 0,
                "{depth:?} frame {index}: {differences} different bytes"
            );
        }
    }
}
