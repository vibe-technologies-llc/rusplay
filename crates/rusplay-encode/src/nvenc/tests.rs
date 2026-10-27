use std::{fs::File, io::Cursor, num::NonZeroU32};

use cros_codecs::codec::{
    h264::parser::{Nalu as H264Nalu, NaluType as H264NaluType},
    h265::parser::{Nalu as H265Nalu, NaluType as H265NaluType},
};
use rusplay_capture::{DrmFourcc, Frame, FrameBuffer, Plane};
use rusplay_config::{
    NvencConfig, RateControl, SoftwareDecoderConfig, VaapiDecoderConfig, VideoConfig,
    VideoEncoderConfig,
};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Chromaticities, Codec, ColorPrimaries, ColorSpace, EncodedFrame,
    PixelFormat, Resolution, Timestamp,
};
use rusplay_decode::{Rav1dDecoder, VaapiDecoder, VideoDecoder};

use crate::{
    Backend, EncodeError, VideoEncoder,
    capabilities::Capabilities,
    hdr::fixtures::{hdr10_color, signalled},
    input::fixtures::yuv_frame,
    nvenc::{
        NvencEncoder, OpaqueMemory, capabilities_with, check_opaque_size,
        driver::{Driver, ExternalMemory, LibraryNames, SYSTEM_LIBRARIES},
        ffi, import_request, layout,
    },
};

const RESOLUTION: Resolution = Resolution::new(640, 360);
const FRAME_INTERVAL_MICROS: u64 = 16_666;
const BITRATE_KBPS: u32 = 20_000;
const MISSING: LibraryNames = LibraryNames {
    cuda: "librusplay-absent-cuda.so.1",
    nvenc: "librusplay-absent-nvidia-encode.so.1",
};

fn yuv420(depth: BitDepth) -> PixelFormat {
    PixelFormat {
        chroma: ChromaSubsampling::Yuv420,
        depth,
    }
}

fn video(codec: Codec, format: PixelFormat) -> VideoConfig {
    VideoConfig {
        resolution: Some(RESOLUTION),
        codec,
        pixel_format: format,
        bitrate_kbps: BITRATE_KBPS,
        encoder: VideoEncoderConfig::Nvenc(NvencConfig::default()),
        ..VideoConfig::default()
    }
}

fn nvenc_of(config: &VideoConfig) -> NvencConfig {
    match &config.encoder {
        VideoEncoderConfig::Nvenc(nvenc) => nvenc.clone(),
        _ => NvencConfig::default(),
    }
}

fn hardware() -> Option<Capabilities> {
    Driver::load(SYSTEM_LIBRARIES).ok()?;
    capabilities_with(&NvencConfig::default(), SYSTEM_LIBRARIES).ok()
}

fn timestamp(index: u64) -> Timestamp {
    Timestamp::from_micros(index * FRAME_INTERVAL_MICROS)
}

fn encode_frames(
    encoder: &mut dyn VideoEncoder,
    format: PixelFormat,
    count: u64,
) -> Vec<EncodedFrame> {
    (0..count)
        .map(|index| {
            let frame = yuv_frame(format, RESOLUTION, index, timestamp(index));
            encoder
                .encode(&frame)
                .unwrap()
                .expect("nvenc returns every frame from the call that submitted it")
        })
        .collect()
}

fn h264_types(data: &[u8]) -> Vec<H264NaluType> {
    let mut cursor = Cursor::new(data);
    let mut types = Vec::new();
    while let Ok(nalu) = H264Nalu::next(&mut cursor) {
        types.push(nalu.header.type_);
    }
    types
}

fn hevc_types(data: &[u8]) -> Vec<H265NaluType> {
    let mut cursor = Cursor::new(data);
    let mut types = Vec::new();
    while let Ok(nalu) = H265Nalu::next(&mut cursor) {
        types.push(nalu.header.type_);
    }
    types
}

fn is_keyframe(codec: Codec, data: &[u8]) -> Option<bool> {
    match codec {
        Codec::H264 => Some(h264_types(data).contains(&H264NaluType::SliceIdr)),
        Codec::Hevc => Some(
            hevc_types(data)
                .iter()
                .any(|kind| matches!(kind, H265NaluType::IdrWRadl | H265NaluType::IdrNLp)),
        ),
        Codec::Av1 => None,
    }
}

#[test]
fn a_missing_driver_fails_open_with_the_library_name() {
    let config = video(Codec::Hevc, yuv420(BitDepth::Eight));

    let Err(error) = NvencEncoder::new_with(&config, &nvenc_of(&config), MISSING) else {
        panic!("an encoder opened without its driver");
    };

    assert!(matches!(
        error,
        EncodeError::LibraryNotFound { library, .. } if library == MISSING.cuda
    ));
}

#[test]
fn a_missing_driver_is_what_capabilities_report() {
    let error = capabilities_with(&NvencConfig::default(), MISSING).unwrap_err();

    assert!(matches!(error, EncodeError::LibraryNotFound { .. }));
}

#[test]
fn the_system_driver_either_answers_or_names_what_is_missing() {
    match crate::capabilities(&VideoEncoderConfig::Nvenc(NvencConfig::default())) {
        Ok(found) => assert_eq!(found.backend(), crate::Backend::Nvenc),
        Err(error) => assert!(
            matches!(
                error,
                EncodeError::LibraryNotFound { .. }
                    | EncodeError::SymbolMissing { .. }
                    | EncodeError::DriverTooOld { .. }
                    | EncodeError::DriverFunctionMissing { .. }
                    | EncodeError::Cuda { .. }
                    | EncodeError::Nvenc { .. }
                    | EncodeError::GpuIndexOutOfRange { .. }
            ),
            "{error}"
        ),
    }
}

#[test]
fn bad_parameters_and_colours_fail_before_the_driver_loads() {
    let mut config = video(Codec::H264, yuv420(BitDepth::Eight));
    let mut nvenc = NvencConfig::default();
    nvenc.params.insert("aq-strength".into(), "99".into());

    let Err(error) = NvencEncoder::new_with(&config, &nvenc, MISSING) else {
        panic!("a bad parameter opened");
    };

    assert!(matches!(error, EncodeError::InvalidParameterValue { .. }));

    config.color = ColorSpace {
        primaries: ColorPrimaries::Custom(Chromaticities::BT709),
        ..ColorSpace::default()
    };

    let Err(error) = NvencEncoder::new_with(&config, &NvencConfig::default(), MISSING) else {
        panic!("custom primaries opened");
    };

    assert!(matches!(
        error,
        EncodeError::ColorPrimariesUnsignalable { .. }
    ));
}

#[test]
fn capabilities_match_what_opens() {
    let Some(found) = hardware() else {
        return;
    };

    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        for chroma in [
            ChromaSubsampling::Yuv420,
            ChromaSubsampling::Yuv422,
            ChromaSubsampling::Yuv444,
        ] {
            for depth in [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve] {
                let format = PixelFormat { chroma, depth };

                assert_eq!(
                    crate::open(&video(codec, format)).is_ok(),
                    found.supports(codec, format),
                    "{codec} {chroma} {}-bit",
                    depth.bits()
                );
            }
        }
    }
}

#[test]
fn streams_start_on_a_keyframe_and_answer_requests() {
    let Some(found) = hardware() else {
        return;
    };

    for codec in found.codecs() {
        let format = yuv420(BitDepth::Eight);
        if !found.supports(codec, format) {
            continue;
        }
        let mut encoder = crate::open(&video(codec, format)).unwrap();

        let mut frames = encode_frames(encoder.as_mut(), format, 4);
        encoder.request_keyframe();
        encoder.set_bitrate(BITRATE_KBPS / 2).unwrap();
        frames.extend(encode_frames(encoder.as_mut(), format, 3));

        let flags: Vec<bool> = frames.iter().map(|frame| frame.keyframe).collect();
        assert_eq!(
            flags,
            [true, false, false, false, true, false, false],
            "{codec}"
        );
        for frame in &frames {
            if let Some(key) = is_keyframe(codec, &frame.data) {
                assert_eq!(key, frame.keyframe, "{codec} bitstream");
            }
        }
    }
}

#[test]
fn colour_description_and_hdr_metadata_reach_the_decoder() {
    let Some(found) = hardware() else {
        return;
    };
    let color = hdr10_color();
    let expected = signalled(&color);

    for (codec, depth) in [
        (Codec::H264, BitDepth::Eight),
        (Codec::Hevc, BitDepth::Ten),
        (Codec::Av1, BitDepth::Ten),
    ] {
        let format = yuv420(depth);
        if !found.supports(codec, format) {
            continue;
        }
        let config = VideoConfig {
            color,
            ..video(codec, format)
        };
        let mut encoder = crate::open(&config).unwrap();
        let packets = encode_frames(encoder.as_mut(), format, 3);

        if let Ok(mut decoder) = VaapiDecoder::new(codec, &VaapiDecoderConfig::default()) {
            for packet in &packets {
                let output = decoder.decode(packet).unwrap().unwrap();
                assert_eq!(output.color, expected, "{codec} through vaapi");
            }
        }
        if codec == Codec::Av1 {
            let mut decoder = Rav1dDecoder::new(&SoftwareDecoderConfig {
                threads: NonZeroU32::new(1),
            })
            .unwrap();
            for packet in &packets {
                let output = decoder.decode(packet).unwrap().unwrap();
                assert_eq!(output.color, expected, "av1 through rav1d");
                assert_eq!(output.resolution, RESOLUTION);
            }
        }
    }
}

#[test]
fn every_rate_control_mode_encodes() {
    let Some(found) = hardware() else {
        return;
    };
    let format = yuv420(BitDepth::Eight);

    for rate_control in [
        RateControl::Cbr,
        RateControl::DEFAULT_VBR,
        RateControl::DEFAULT_CONSTANT_QUALITY,
    ] {
        for codec in found.codecs() {
            let config = VideoConfig {
                encoder: VideoEncoderConfig::Nvenc(NvencConfig {
                    rate_control,
                    ..NvencConfig::default()
                }),
                ..video(codec, format)
            };
            let Ok(mut encoder) = crate::open(&config) else {
                continue;
            };

            assert_eq!(encode_frames(encoder.as_mut(), format, 2).len(), 2);
        }
    }
}

#[test]
fn unspecified_resolution_follows_the_first_frame() {
    let Some(found) = hardware() else {
        return;
    };
    let format = yuv420(BitDepth::Eight);
    let Some(codec) = found.codecs().into_iter().next() else {
        return;
    };

    let config = VideoConfig {
        resolution: None,
        ..video(codec, format)
    };
    let mut encoder = crate::open(&config).unwrap();
    let first = yuv_frame(format, RESOLUTION, 0, timestamp(0));
    let other = yuv_frame(format, Resolution::new(320, 180), 1, timestamp(1));

    assert!(encoder.encode(&first).unwrap().unwrap().keyframe);
    assert!(matches!(
        encoder.encode(&other),
        Err(EncodeError::FrameResolutionMismatch { .. })
    ));
}

fn opaque_frame(format: PixelFormat, resolution: Resolution, size: u64) -> Frame {
    let pitch = (resolution.width * 2).next_multiple_of(256);
    let plane_bytes = pitch * resolution.height;
    let planes = (0..layout::Layout::of(format).planes())
        .map(|index| Plane {
            offset: u32::try_from(index).unwrap() * plane_bytes,
            stride: pitch,
        })
        .collect();

    Frame {
        resolution,
        format: layout::dma_buf_fourcc(format),
        color: ColorSpace::default(),
        timestamp: Timestamp::ZERO,
        buffer: FrameBuffer::OpaqueFd {
            fd: File::open("/dev/null").unwrap().into(),
            allocation: 7,
            size,
            dedicated: true,
            planes,
        },
    }
}

#[test]
fn opaque_fd_frames_are_imported_with_their_allocation() {
    for format in [
        yuv420(BitDepth::Eight),
        yuv420(BitDepth::Ten),
        PixelFormat {
            chroma: ChromaSubsampling::Yuv422,
            depth: BitDepth::Eight,
        },
        PixelFormat {
            chroma: ChromaSubsampling::Yuv422,
            depth: BitDepth::Ten,
        },
        PixelFormat {
            chroma: ChromaSubsampling::Yuv444,
            depth: BitDepth::Eight,
        },
        PixelFormat {
            chroma: ChromaSubsampling::Yuv444,
            depth: BitDepth::Ten,
        },
    ] {
        let frame = opaque_frame(format, RESOLUTION, 4 << 20);

        let request = import_request(&frame, format).unwrap();
        let layout = layout::dma_buf_layout(format, RESOLUTION, request.planes).unwrap();
        let opaque = request.opaque.unwrap();

        assert_eq!(
            opaque,
            OpaqueMemory {
                allocation: 7,
                size: 4 << 20,
                dedicated: true,
            }
        );
        assert!(check_opaque_size(opaque, layout).is_ok());
        assert!(matches!(
            check_opaque_size(
                OpaqueMemory {
                    size: layout.len - 1,
                    ..opaque
                },
                layout
            ),
            Err(EncodeError::OpaqueFdTooShort { needed, actual })
                if needed == layout.len && actual == layout.len - 1
        ));
    }
}

#[test]
fn opaque_fd_frames_of_another_format_are_refused() {
    let format = yuv420(BitDepth::Eight);
    let mut frame = opaque_frame(yuv420(BitDepth::Ten), RESOLUTION, 4 << 20);

    let wrong = import_request(&frame, format).unwrap_err();
    frame.format = layout::dma_buf_fourcc(format);
    frame.buffer = FrameBuffer::Shm {
        data: Vec::new(),
        stride: RESOLUTION.width,
    };
    let shm = import_request(&frame, format).unwrap_err();

    assert!(matches!(
        wrong,
        EncodeError::FrameFormatMismatch {
            backend: Backend::Nvenc,
            expected,
            actual,
        } if expected == DrmFourcc::from_code(*b"NV12") && actual == DrmFourcc::from_code(*b"P010")
    ));
    assert!(matches!(
        shm,
        EncodeError::CpuFrameRequired {
            backend: Backend::Nvenc
        }
    ));
}

#[test]
fn dedicated_allocations_are_flagged_to_cuda() {
    let dedicated = ExternalMemory {
        size: 1 << 20,
        dedicated: true,
    }
    .handle_desc(5);
    let shared = ExternalMemory {
        size: 1 << 20,
        dedicated: false,
    }
    .handle_desc(5);

    assert_eq!(
        dedicated.handle_type,
        ffi::CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD
    );
    assert_eq!(unsafe { dedicated.handle.fd }, 5);
    assert_eq!(dedicated.size, 1 << 20);
    assert_eq!(dedicated.flags, ffi::CUDA_EXTERNAL_MEMORY_DEDICATED);
    assert_eq!(shared.flags, 0);
}
