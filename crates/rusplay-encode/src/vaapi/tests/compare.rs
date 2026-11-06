use super::*;

const MOTION_RESOLUTION: Resolution = Resolution::new(2560, 1440);
const MOTION_FRAMES: u64 = 64;

fn software_reference(format: PixelFormat, packets: &[EncodedFrame]) -> Option<Vec<u8>> {
    let planar = if format.depth == BitDepth::Eight {
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
            planar,
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
            hardware::unsupported(format_args!("the ffmpeg binary: {error}"));
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

fn motion_frames(depth: BitDepth) -> Option<Vec<Frame>> {
    let planar = if depth == BitDepth::Eight {
        "yuv420p"
    } else {
        "yuv420p10le"
    };
    let output = match Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=2560x1440:rate=60",
            "-frames:v",
            "64",
            "-threads",
            "1",
            "-pix_fmt",
            planar,
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            hardware::unsupported(format_args!("the ffmpeg binary: {error}"));
            return None;
        }
    };
    assert!(
        output.status.success(),
        "ffmpeg source: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let format = yuv420(depth);
    let sample = bytes_per_sample(depth);
    let frame_bytes = usize::try_from(MOTION_RESOLUTION.pixel_count()).unwrap() * sample * 3 / 2;

    Some(
        output
            .stdout
            .chunks_exact(frame_bytes)
            .enumerate()
            .map(|(index, bytes)| Frame {
                resolution: MOTION_RESOLUTION,
                format: fourcc_for(format),
                timestamp: timestamp(u64::try_from(index).unwrap()),
                color: ColorSpace::default(),
                buffer: FrameBuffer::Shm {
                    data: bytes.to_vec().into(),
                    stride: MOTION_RESOLUTION.width * u32::try_from(sample).unwrap(),
                },
            })
            .collect(),
    )
}

#[test]
fn moving_hevc_luma_and_chroma_match_software_decoding() {
    for depth in [BitDepth::Eight, BitDepth::Ten] {
        let format = yuv420(depth);
        let config = with_vaapi(video(Codec::Hevc, format, MOTION_RESOLUTION), |vaapi| {
            vaapi.low_power = false;
            vaapi.rate_control = RateControl::ConstantQuality { qp: 20 };
        });
        let Some(mut encoder) = try_open(&config) else {
            continue;
        };
        let Some(frames) = motion_frames(depth) else {
            return;
        };
        let packets = encode_all(encoder.as_mut(), &frames);
        drop(frames);

        assert_eq!(packets.len(), usize::try_from(MOTION_FRAMES).unwrap());
        assert_software_reference("motion", depth, &packets);
    }
}

fn assert_software_reference(name: &str, depth: BitDepth, packets: &[EncodedFrame]) {
    let format = yuv420(depth);
    let Some(expected) = software_reference(format, packets) else {
        return;
    };
    write_fixture(name, depth, packets, &expected);
    let mut decoder = VaapiDecoder::new(
        Codec::Hevc,
        &VaapiDecoderConfig {
            copy_output: true,
            ..VaapiDecoderConfig::default()
        },
    )
    .unwrap();
    let sample = bytes_per_sample(depth);
    let luma_bytes = usize::try_from(MOTION_RESOLUTION.pixel_count()).unwrap() * sample;
    let frame_bytes = luma_bytes * 3 / 2;

    assert_eq!(expected.len(), frame_bytes * packets.len());
    for (index, (packet, expected)) in packets
        .iter()
        .zip(expected.chunks_exact(frame_bytes))
        .enumerate()
    {
        let output = decoder.decode(packet).unwrap().unwrap();
        let DecodedBuffer::Cpu(cpu) = &output.buffer else {
            panic!("expected CPU output")
        };
        let mut offset = 0;
        for (plane, data) in cpu.planes().enumerate() {
            let row_bytes = data.width as usize * sample;
            let actual: Vec<u8> = data
                .data
                .chunks_exact(data.stride)
                .take(data.height as usize)
                .flat_map(|row| row[..row_bytes].iter().copied())
                .collect();
            let reference = &expected[offset..offset + actual.len()];
            let differences = actual
                .iter()
                .zip(reference)
                .filter(|(left, right)| left != right)
                .count();

            assert_eq!(
                differences,
                0,
                "{depth:?} frame {index} plane {plane}: {differences} different bytes of {}",
                actual.len()
            );
            offset += actual.len();
        }
        assert_eq!(offset, frame_bytes);
    }
}

fn write_fixture(name: &str, depth: BitDepth, packets: &[EncodedFrame], raw: &[u8]) {
    let Some(root) = std::env::var_os("RUSPLAY_HEVC_FIXTURE_DIR") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    std::fs::create_dir_all(&root).unwrap();
    let stem = format!("hevc-rusplay-{name}-{}bit", depth.bits());
    let encoded: Vec<_> = packets
        .iter()
        .flat_map(|packet| packet.data.iter().copied())
        .collect();
    std::fs::write(root.join(format!("{stem}.hevc")), encoded).unwrap();
    let mut index = File::create(root.join(format!("{stem}.index"))).unwrap();
    let luma = usize::try_from(MOTION_RESOLUTION.pixel_count()).unwrap() * bytes_per_sample(depth);
    let frame_bytes = luma * 3 / 2;
    let hash = |bytes: &[u8]| {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };

    for (packet, frame) in packets.iter().zip(raw.chunks_exact(frame_bytes)) {
        writeln!(
            index,
            "{} {:016x} {:016x} {:016x}",
            packet.data.len(),
            hash(&frame[..luma]),
            hash(&frame[luma..luma * 5 / 4]),
            hash(&frame[luma * 5 / 4..])
        )
        .unwrap();
    }
}

#[test]
fn recovered_hevc_luma_and_chroma_match_software_decoding() {
    if !device_present() || !long_term_verified() {
        return;
    }
    let depth = BitDepth::Ten;
    let format = yuv420(depth);
    let config = with_vaapi(video(Codec::Hevc, format, MOTION_RESOLUTION), |vaapi| {
        vaapi.low_power = false;
        vaapi.rate_control = RateControl::ConstantQuality { qp: 20 };
    });
    let Some(mut encoder) = try_open(&config) else {
        return;
    };
    let Some(frames) = motion_frames(depth) else {
        return;
    };
    let mut packets = encode_all(encoder.as_mut(), &frames[..32]);
    let recovery = encoder.recover(OutputIndex(20));
    packets.extend(encode_all(encoder.as_mut(), &frames[32..]));
    drop(frames);
    let received = pick(
        &packets,
        &(0..=20).chain(32..MOTION_FRAMES).collect::<Vec<_>>(),
    );

    assert!(matches!(recovery, Recovery::Reference(_)));
    assert_software_reference("recovery", depth, &received);
}
