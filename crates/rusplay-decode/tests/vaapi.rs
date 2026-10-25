#![cfg(feature = "vaapi")]

mod common;

use std::path::Path;

use rusplay_config::VaapiDecoderConfig;
use rusplay_core::{BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution};
use rusplay_decode::{Backend, DecodeError, DecodedBuffer, DrmFourcc, VaapiDecoder, VideoDecoder};

const FIXTURE_RESOLUTION: Resolution = Resolution::new(256, 144);
const FIXTURE_FRAMES: usize = 8;
const REORDER_DELAY_LIMIT: usize = 3;

fn open(codec: Codec) -> Option<VaapiDecoder> {
    let config = VaapiDecoderConfig::default();
    if !Path::new(&config.device).exists() {
        return None;
    }

    match VaapiDecoder::new(codec, &config) {
        Ok(decoder) => Some(decoder),
        Err(DecodeError::CodecUnsupported { .. }) => None,
        Err(error) => panic!("opening the {codec} vaapi decoder: {error}"),
    }
}

fn decode_every_packet(
    codec: Codec,
    frames: &[EncodedFrame],
    expected: PixelFormat,
    fourcc: DrmFourcc,
) {
    let Some(mut decoder) = open(codec) else {
        return;
    };

    assert_eq!(frames.len(), FIXTURE_FRAMES);

    let mut held = Vec::new();
    for frame in frames {
        let output = match decoder.decode(frame) {
            Ok(Some(output)) => output,
            Ok(None) => panic!("no picture for the packet at {:?}", frame.timestamp),
            Err(
                DecodeError::ChromaUnsupported { .. } | DecodeError::BitDepthUnsupported { .. },
            ) => {
                return;
            }
            Err(error) => panic!("decoding the packet at {:?}: {error}", frame.timestamp),
        };

        assert_eq!(output.resolution, FIXTURE_RESOLUTION);
        assert_eq!(output.format, expected);
        assert_eq!(output.timestamp, frame.timestamp);

        let DecodedBuffer::DmaBuf(dma_buf) = &output.buffer else {
            panic!("vaapi produced a non-DMA-BUF buffer: {:?}", output.buffer);
        };
        let image = dma_buf.image();

        assert_eq!(image.fourcc, fourcc);
        assert!(image.width >= FIXTURE_RESOLUTION.width);
        assert!(image.height >= FIXTURE_RESOLUTION.height);
        assert_eq!(image.planes.len(), 2);
        for (plane, layout) in image.planes.iter().enumerate() {
            assert!(image.plane_fd(plane).is_some());
            assert!(layout.pitch >= u64::from(FIXTURE_RESOLUTION.width));
        }

        held.push(output);
    }

    let surfaces: Vec<_> = held
        .iter()
        .map(|output| match &output.buffer {
            DecodedBuffer::DmaBuf(dma_buf) => std::ptr::from_ref(dma_buf.image()),
            DecodedBuffer::Cpu(_) => std::ptr::null(),
        })
        .collect();
    for (index, surface) in surfaces.iter().enumerate() {
        assert!(
            !surfaces[..index].contains(surface),
            "a held frame's surface was reused"
        );
    }
}

fn eight_bit_420() -> PixelFormat {
    PixelFormat {
        chroma: ChromaSubsampling::Yuv420,
        depth: BitDepth::Eight,
    }
}

fn ten_bit_420() -> PixelFormat {
    PixelFormat {
        chroma: ChromaSubsampling::Yuv420,
        depth: BitDepth::Ten,
    }
}

#[test]
fn decodes_h264_one_picture_per_packet() {
    decode_every_packet(
        Codec::H264,
        &common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit.h264")),
        eight_bit_420(),
        DrmFourcc::NV12,
    );
}

#[test]
fn rejects_frames_of_another_codec() {
    let Some(mut decoder) = open(Codec::H264) else {
        return;
    };
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
fn missing_device_is_reported() {
    let config = VaapiDecoderConfig {
        device: "/dev/dri/rusplay-missing-node".into(),
    };

    assert!(matches!(
        VaapiDecoder::new(Codec::H264, &config),
        Err(DecodeError::DeviceOpen { .. })
    ));
}

#[test]
fn decodes_hevc_one_picture_per_packet() {
    decode_every_packet(
        Codec::Hevc,
        &common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-8bit.hevc")),
        eight_bit_420(),
        DrmFourcc::NV12,
    );
}

#[test]
fn decodes_hevc_main10_into_p010() {
    decode_every_packet(
        Codec::Hevc,
        &common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-10bit.hevc")),
        ten_bit_420(),
        DrmFourcc::P010,
    );
}

#[test]
fn decodes_av1_one_picture_per_packet() {
    decode_every_packet(
        Codec::Av1,
        &common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit.ivf")),
        eight_bit_420(),
        DrmFourcc::NV12,
    );
}

#[test]
fn decodes_10_bit_av1_into_p010() {
    decode_every_packet(
        Codec::Av1,
        &common::ivf_frames(Codec::Av1, &common::fixture("av1-420-10bit.ivf")),
        ten_bit_420(),
        DrmFourcc::P010,
    );
}

#[test]
fn hevc_422_decodes_or_reports_the_chroma() {
    let Some(mut decoder) = open(Codec::Hevc) else {
        return;
    };
    let frames = common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-422-8bit.hevc"));

    match decoder.decode(&frames[0]) {
        Ok(Some(output)) => assert_eq!(output.format.chroma, ChromaSubsampling::Yuv422),
        Err(DecodeError::ChromaUnsupported {
            backend: Backend::Vaapi,
            codec: Codec::Hevc,
            chroma: ChromaSubsampling::Yuv422,
        }) => {}
        other => panic!("unexpected result for 4:2:2 hevc: {other:?}"),
    }
}

fn every_packet_comes_out_once_after_a_flush(codec: Codec, name: &str, frames: usize) {
    let Some(mut decoder) = open(codec) else {
        return;
    };
    let packets = common::annexb_access_units(codec, &common::fixture(name));
    assert_eq!(packets.len(), frames);

    let mut outputs = Vec::new();
    for packet in &packets {
        outputs.extend(decoder.decode(packet).unwrap());
    }
    let before_flush = outputs.len();
    outputs.extend(decoder.flush().unwrap());

    assert!(before_flush < frames);
    assert!(before_flush + REORDER_DELAY_LIMIT >= frames);
    assert!(decoder.flush().unwrap().is_empty());

    let mut timestamps: Vec<_> = outputs.iter().map(|output| output.timestamp).collect();
    timestamps.sort_unstable();
    let mut expected: Vec<_> = packets.iter().map(|packet| packet.timestamp).collect();
    expected.sort_unstable();
    assert_eq!(timestamps, expected);
}

#[test]
fn h264_with_b_frames_holds_a_few_pictures_and_flushes_them() {
    every_packet_comes_out_once_after_a_flush(Codec::H264, "h264-420-8bit-bframes.h264", 12);
}

#[test]
fn hevc_with_b_frames_holds_a_few_pictures_and_flushes_them() {
    every_packet_comes_out_once_after_a_flush(Codec::Hevc, "hevc-420-8bit-bframes.hevc", 12);
}

#[test]
fn streams_without_reordering_flush_nothing() {
    let Some(mut decoder) = open(Codec::H264) else {
        return;
    };
    for packet in common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit.h264")) {
        assert!(decoder.decode(&packet).unwrap().is_some());
    }

    assert!(decoder.flush().unwrap().is_empty());
}
