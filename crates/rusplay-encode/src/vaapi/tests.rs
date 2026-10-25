use std::{
    fs::File,
    io::Cursor,
    num::NonZeroU32,
    os::fd::{AsFd, OwnedFd},
    path::Path,
    rc::Rc,
};

use cros_codecs::codec::{
    av1::parser::{FrameType, ObuAction, ParsedObu, Parser as Av1Parser},
    h264::parser::{Nalu as H264Nalu, NaluType as H264NaluType, Parser as H264Parser},
    h265::parser::{Nalu as H265Nalu, NaluType as H265NaluType, Parser as H265Parser},
};
use rusplay_capture::{DrmFourcc, Frame, FrameBuffer, Plane};
use rusplay_config::{
    RateControl, SoftwareDecoderConfig, VaapiConfig, VaapiDecoderConfig, VideoConfig,
    VideoEncoderConfig,
};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorPrimaries, ColorRange, ColorSpace, EncodedFrame,
    FrameRate, PixelFormat, Resolution, Timestamp, TransferFunction,
};
use rusplay_decode::{DecodedBuffer, DecodedFrame, Rav1dDecoder, VaapiDecoder, VideoDecoder};

use crate::{
    EncodeError, VideoEncoder, capabilities,
    hdr::fixtures::{hdr10_color, signalled},
    input::{bytes_per_sample, fixtures::yuv_frame, fourcc_for},
    open,
    vaapi::{
        StreamInfo,
        av1::{Av1Profile, Av1Stream, TileLayout},
        format::{pack, surface_format},
        h264::{H264Profile, H264Stream},
        hevc::{HevcProfile, HevcStream},
        va::{Display, DmaBufImport, Image, Surface},
    },
};

const RESOLUTION: Resolution = Resolution::new(400, 216);
const ODD_RESOLUTION: Resolution = Resolution::new(402, 226);
const FRAME_INTERVAL_MICROS: u64 = 16_666;
const FRAMES: u64 = 12;
const BITRATE_KBPS: u32 = 20_000;
const PSNR_FLOOR_DB: f64 = 34.0;

fn pixel(chroma: ChromaSubsampling, depth: BitDepth) -> PixelFormat {
    PixelFormat { chroma, depth }
}

fn yuv420(depth: BitDepth) -> PixelFormat {
    pixel(ChromaSubsampling::Yuv420, depth)
}

fn video(codec: Codec, format: PixelFormat, resolution: Resolution) -> VideoConfig {
    VideoConfig {
        resolution: Some(resolution),
        codec,
        pixel_format: format,
        bitrate_kbps: BITRATE_KBPS,
        encoder: VideoEncoderConfig::Vaapi(VaapiConfig::default()),
        ..VideoConfig::default()
    }
}

fn with_vaapi(config: VideoConfig, edit: impl FnOnce(&mut VaapiConfig)) -> VideoConfig {
    let mut vaapi = VaapiConfig::default();
    edit(&mut vaapi);

    VideoConfig {
        encoder: VideoEncoderConfig::Vaapi(vaapi),
        ..config
    }
}

fn device_present() -> bool {
    Path::new(&VaapiConfig::default().device).exists()
}

fn try_open(config: &VideoConfig) -> Option<Box<dyn VideoEncoder>> {
    if !device_present() {
        return None;
    }

    match open(config) {
        Ok(encoder) => Some(encoder),
        Err(
            EncodeError::CodecUnsupported { .. }
            | EncodeError::ChromaUnsupported { .. }
            | EncodeError::BitDepthUnsupported { .. }
            | EncodeError::ResolutionUnsupported { .. },
        ) => None,
        Err(error) => panic!("opening the {} vaapi encoder: {error}", config.codec),
    }
}

fn timestamp(index: u64) -> Timestamp {
    Timestamp::from_micros(index * FRAME_INTERVAL_MICROS)
}

fn pattern_value(
    x: usize,
    y: usize,
    index: u64,
    plane: usize,
    size: (usize, usize),
    max: u64,
) -> u64 {
    let (width, height) = size;
    let shift = usize::try_from(index * 3).unwrap_or(0);
    let horizontal = ((x + shift) % width) as u64 * max / width as u64;
    let vertical = y as u64 * max / height.max(1) as u64;

    match plane {
        0 => (horizontal * 3 + vertical) / 4,
        1 => (max / 4 + vertical / 2).min(max),
        _ => (max * 3 / 4).saturating_sub(horizontal / 2),
    }
}

fn pattern_planes(format: PixelFormat, resolution: Resolution, index: u64) -> Vec<Vec<u16>> {
    let width = resolution.width as usize;
    let height = resolution.height as usize;
    let (chroma_width, chroma_height) = match format.chroma {
        ChromaSubsampling::Yuv420 => (width.div_ceil(2), height.div_ceil(2)),
        ChromaSubsampling::Yuv422 => (width.div_ceil(2), height),
        ChromaSubsampling::Yuv444 => (width, height),
    };
    let max = (1_u64 << format.depth.bits()) - 1;

    [
        (width, height),
        (chroma_width, chroma_height),
        (chroma_width, chroma_height),
    ]
    .into_iter()
    .enumerate()
    .map(|(plane, (plane_width, rows))| {
        (0..rows)
            .flat_map(|y| {
                (0..plane_width).map(move |x| {
                    let value = pattern_value(x, y, index, plane, (plane_width, rows), max);
                    u16::try_from(value).unwrap_or(u16::MAX)
                })
            })
            .collect()
    })
    .collect()
}

fn pattern_frame(format: PixelFormat, resolution: Resolution, index: u64) -> Frame {
    let sample = bytes_per_sample(format.depth);
    let data = pattern_planes(format, resolution, index)
        .into_iter()
        .flatten()
        .flat_map(|value| value.to_le_bytes().into_iter().take(sample))
        .collect();

    Frame {
        resolution,
        format: fourcc_for(format),
        timestamp: timestamp(index),
        color: ColorSpace::default(),
        buffer: FrameBuffer::Shm {
            data,
            stride: u32::try_from(resolution.width as usize * sample).unwrap_or(0),
        },
    }
}

fn encode_all(encoder: &mut dyn VideoEncoder, frames: &[Frame]) -> Vec<EncodedFrame> {
    frames
        .iter()
        .map(|frame| {
            encoder
                .encode(frame)
                .unwrap()
                .expect("every frame comes out of the call that submitted it")
        })
        .collect()
}

fn pattern_frames(format: PixelFormat, resolution: Resolution, count: u64) -> Vec<Frame> {
    (0..count)
        .map(|index| pattern_frame(format, resolution, index))
        .collect()
}

#[derive(Debug)]
struct Picture {
    resolution: Resolution,
    format: PixelFormat,
    luma: Vec<u16>,
}

fn read_back(display: &Rc<Display>, output: &DecodedFrame) -> Picture {
    let DecodedBuffer::DmaBuf(dma_buf) = &output.buffer else {
        panic!("vaapi decode produced a CPU frame");
    };
    let image = dma_buf.image();
    assert_eq!(
        image.objects.len(),
        1,
        "multi-object exports are not read back"
    );

    let format = output.format;
    let layouts: Vec<_> = image
        .planes
        .iter()
        .map(|plane| crate::vaapi::va::DmaBufPlaneLayout {
            offset: u32::try_from(plane.offset).unwrap(),
            pitch: u32::try_from(plane.pitch).unwrap(),
        })
        .collect();
    let surface = Surface::import(
        display,
        surface_format(format),
        &DmaBufImport {
            fd: image.objects[0].fd.as_fd(),
            size: u32::try_from(image.objects[0].size).unwrap(),
            modifier: image.modifier,
            resolution: Resolution::new(image.width, image.height),
            planes: &layouts,
        },
    )
    .unwrap();

    let mut va_image =
        Image::new(display, surface_format(format).va_fourcc, output.resolution).unwrap();
    let bytes = va_image.get(&surface).unwrap();
    let layout = va_image.image_layout();

    let wide = format.depth != BitDepth::Eight;
    let shift = 16 - u32::from(format.depth.bits());
    let width = output.resolution.width as usize;
    let luma = (0..output.resolution.height as usize)
        .flat_map(|y| {
            let row = layout.offsets[0] + y * layout.pitches[0];
            let bytes = &bytes;
            (0..width).map(move |x| {
                if wide {
                    u16::from_le_bytes([bytes[row + 2 * x], bytes[row + 2 * x + 1]]) >> shift
                } else {
                    u16::from(bytes[row + x])
                }
            })
        })
        .collect();

    Picture {
        resolution: output.resolution,
        format,
        luma,
    }
}

fn decode_vaapi(codec: Codec, frames: &[EncodedFrame]) -> Option<Vec<Picture>> {
    let mut decoder = match VaapiDecoder::new(codec, &VaapiDecoderConfig::default()) {
        Ok(decoder) => decoder,
        Err(rusplay_decode::DecodeError::CodecUnsupported { .. }) => return None,
        Err(error) => panic!("opening the {codec} vaapi decoder: {error}"),
    };
    let display = Display::open(&VaapiConfig::default().device).unwrap();

    let pictures = frames
        .iter()
        .map(|frame| {
            let output = match decoder.decode(frame) {
                Ok(Some(output)) => output,
                Ok(None) => panic!("no picture for the {codec} packet at {:?}", frame.timestamp),
                Err(error) => panic!(
                    "decoding the {codec} packet at {:?}: {error}",
                    frame.timestamp
                ),
            };
            assert_eq!(output.timestamp, frame.timestamp);
            read_back(&display, &output)
        })
        .collect();

    Some(pictures)
}

fn decode_rav1d(frames: &[EncodedFrame]) -> Vec<Picture> {
    let config = SoftwareDecoderConfig {
        threads: NonZeroU32::new(1),
    };
    let mut decoder = Rav1dDecoder::new(&config).unwrap();

    frames
        .iter()
        .map(|frame| {
            let output = decoder
                .decode(frame)
                .unwrap()
                .expect("rav1d outputs every frame");
            let DecodedBuffer::Cpu(cpu) = &output.buffer else {
                panic!("rav1d produced a DMA-BUF");
            };
            let plane = cpu.plane(0).unwrap();
            let sample = usize::from(plane.bytes_per_sample);
            let luma = (0..plane.height as usize)
                .flat_map(|y| {
                    (0..plane.width as usize).map(move |x| {
                        let at = y * plane.stride + x * sample;
                        if sample == 2 {
                            u16::from_le_bytes([plane.data[at], plane.data[at + 1]])
                        } else {
                            u16::from(plane.data[at])
                        }
                    })
                })
                .collect();

            Picture {
                resolution: output.resolution,
                format: output.format,
                luma,
            }
        })
        .collect()
}

fn psnr(source: &[u16], output: &[u16], depth: BitDepth) -> f64 {
    let max = f64::from((1_u32 << depth.bits()) - 1);
    let squared: f64 = source
        .iter()
        .zip(output)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum();
    let mse = squared / f64::from(u32::try_from(source.len()).unwrap());

    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (max * max / mse).log10()
    }
}

fn check_pictures(codec: Codec, pictures: &[Picture], format: PixelFormat, resolution: Resolution) {
    for (index, picture) in pictures.iter().enumerate() {
        let source = &pattern_planes(format, resolution, index as u64)[0];
        let quality = psnr(source, &picture.luma, format.depth);

        assert_eq!(picture.resolution, resolution, "{codec} frame {index}");
        assert_eq!(picture.format, format, "{codec} frame {index}");
        assert!(
            quality >= PSNR_FLOOR_DB,
            "{codec} frame {index} decodes at {quality:.1} dB, below {PSNR_FLOOR_DB} dB"
        );
    }
}

fn h264_nal_types(data: &[u8]) -> Vec<H264NaluType> {
    let mut cursor = Cursor::new(data);
    let mut types = Vec::new();
    while let Ok(nalu) = H264Nalu::next(&mut cursor) {
        types.push(nalu.header.type_);
    }
    types
}

fn hevc_nal_types(data: &[u8]) -> Vec<H265NaluType> {
    let mut cursor = Cursor::new(data);
    let mut types = Vec::new();
    while let Ok(nalu) = H265Nalu::next(&mut cursor) {
        types.push(nalu.header.type_);
    }
    types
}

fn av1_frame_types(data: &[u8]) -> (bool, Vec<FrameType>) {
    let mut parser = Av1Parser::default();
    let mut has_sequence_header = false;
    let mut types = Vec::new();
    let mut rest = data;

    while !rest.is_empty() {
        let obu = match parser.read_obu(rest).unwrap() {
            ObuAction::Process(obu) => obu,
            ObuAction::Drop(length) => {
                rest = &rest[length as usize..];
                continue;
            }
        };
        rest = &rest[obu.bytes_used..];
        match parser
            .parse_obu(obu)
            .unwrap_or_else(|error| panic!("parsing an obu: {error}"))
        {
            ParsedObu::SequenceHeader(_) => has_sequence_header = true,
            ParsedObu::FrameHeader(header) => types.push(header.frame_type),
            ParsedObu::Frame(frame) => types.push(frame.header.frame_type),
            _ => {}
        }
    }

    (has_sequence_header, types)
}

fn is_h264_idr(data: &[u8]) -> bool {
    h264_nal_types(data).contains(&H264NaluType::SliceIdr)
}

fn is_hevc_idr(data: &[u8]) -> bool {
    hevc_nal_types(data)
        .iter()
        .any(|kind| matches!(kind, H265NaluType::IdrWRadl | H265NaluType::IdrNLp))
}

fn av1_obus(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut obus = Vec::new();
    let mut rest = data;

    while let [header, tail @ ..] = rest {
        let kind = (header >> 3) & 0x0f;
        let mut size = 0;
        let mut used = 0;
        for (index, byte) in tail.iter().enumerate() {
            size |= usize::from(byte & 0x7f) << (7 * index);
            used = index + 1;
            if byte & 0x80 == 0 {
                break;
            }
        }
        obus.push((kind, &tail[used..used + size]));
        rest = &tail[used + size..];
    }

    obus
}

fn is_av1_key(data: &[u8]) -> bool {
    av1_obus(data)
        .iter()
        .any(|&(kind, payload)| matches!(kind, 3 | 6) && payload[0] >> 5 == 0)
}

fn keyframe_check(codec: Codec) -> fn(&[u8]) -> bool {
    match codec {
        Codec::H264 => is_h264_idr,
        Codec::Hevc => is_hevc_idr,
        Codec::Av1 => is_av1_key,
    }
}

#[test]
fn streams_start_on_a_keyframe_and_stay_inter() {
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let format = yuv420(BitDepth::Eight);
        let Some(mut encoder) = try_open(&video(codec, format, RESOLUTION)) else {
            continue;
        };

        let packets = encode_all(
            encoder.as_mut(),
            &pattern_frames(format, RESOLUTION, FRAMES),
        );
        let is_key = keyframe_check(codec);

        for (index, frame) in packets.iter().enumerate() {
            let first = index == 0;

            assert_eq!(frame.codec, codec);
            assert_eq!(frame.timestamp, timestamp(index as u64));
            assert_eq!(frame.keyframe, first, "{codec} frame {index}");
            assert_eq!(is_key(&frame.data), first, "{codec} frame {index}");
        }
        match codec {
            Codec::H264 => {
                let types = h264_nal_types(&packets[0].data);
                assert!(types.contains(&H264NaluType::Sps));
                assert!(types.contains(&H264NaluType::Pps));
            }
            Codec::Hevc => {
                let types = hevc_nal_types(&packets[0].data);
                assert!(types.contains(&H265NaluType::VpsNut));
                assert!(types.contains(&H265NaluType::SpsNut));
                assert!(types.contains(&H265NaluType::PpsNut));
            }
            Codec::Av1 => {
                assert!(
                    av1_obus(&packets[0].data)
                        .iter()
                        .any(|&(kind, _)| kind == 1)
                );
                assert!(
                    packets[1..]
                        .iter()
                        .all(|frame| av1_obus(&frame.data).iter().all(|&(kind, _)| kind != 1))
                );
                assert!(packets.iter().all(|frame| frame.data[..2] == [0x12, 0x00]));
            }
        }
    }
}

#[test]
fn decoded_frames_match_the_source() {
    let cases = [
        (Codec::H264, BitDepth::Eight),
        (Codec::Hevc, BitDepth::Eight),
        (Codec::Hevc, BitDepth::Ten),
        (Codec::Av1, BitDepth::Eight),
        (Codec::Av1, BitDepth::Ten),
    ];

    for (codec, depth) in cases {
        let format = yuv420(depth);
        let Some(mut encoder) = try_open(&video(codec, format, RESOLUTION)) else {
            continue;
        };

        let packets = encode_all(
            encoder.as_mut(),
            &pattern_frames(format, RESOLUTION, FRAMES),
        );

        if let Some(pictures) = decode_vaapi(codec, &packets) {
            check_pictures(codec, &pictures, format, RESOLUTION);
        }
        if codec == Codec::Av1 {
            check_pictures(codec, &decode_rav1d(&packets), format, RESOLUTION);
        }
    }
}

#[test]
fn colour_description_and_hdr_metadata_reach_the_decoder() {
    let cases = [
        (Codec::H264, BitDepth::Eight),
        (Codec::Hevc, BitDepth::Ten),
        (Codec::Av1, BitDepth::Ten),
    ];
    let color = hdr10_color();
    let expected = signalled(&color);

    for (codec, depth) in cases {
        let format = yuv420(depth);
        let config = VideoConfig {
            color,
            ..video(codec, format, RESOLUTION)
        };
        let Some(mut encoder) = try_open(&config) else {
            continue;
        };
        let packets = encode_all(encoder.as_mut(), &pattern_frames(format, RESOLUTION, 3));

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
            }
        }
    }
}

#[test]
fn sizes_off_the_block_grid_are_cropped() {
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let format = yuv420(BitDepth::Eight);
        let Some(mut encoder) = try_open(&video(codec, format, ODD_RESOLUTION)) else {
            continue;
        };

        let packets = encode_all(encoder.as_mut(), &pattern_frames(format, ODD_RESOLUTION, 4));

        if let Some(pictures) = decode_vaapi(codec, &packets) {
            check_pictures(codec, &pictures, format, ODD_RESOLUTION);
        }
        if codec == Codec::Av1 {
            check_pictures(codec, &decode_rav1d(&packets), format, ODD_RESOLUTION);
        }
    }
}

#[test]
fn keyframe_is_produced_on_request() {
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let format = yuv420(BitDepth::Eight);
        let Some(mut encoder) = try_open(&video(codec, format, RESOLUTION)) else {
            continue;
        };
        let is_key = keyframe_check(codec);

        for index in 0..3 {
            encoder
                .encode(&pattern_frame(format, RESOLUTION, index))
                .unwrap();
        }
        encoder.request_keyframe();
        let forced = encoder
            .encode(&pattern_frame(format, RESOLUTION, 3))
            .unwrap()
            .unwrap();
        let after = encoder
            .encode(&pattern_frame(format, RESOLUTION, 4))
            .unwrap()
            .unwrap();

        assert!(forced.keyframe);
        assert!(is_key(&forced.data), "{codec}");
        assert!(!after.keyframe);
        assert!(!is_key(&after.data), "{codec}");
    }
}

fn mean_size(frames: &[EncodedFrame]) -> usize {
    frames.iter().map(|frame| frame.data.len()).sum::<usize>() / frames.len().max(1)
}

#[test]
fn bitrate_changes_apply_in_place() {
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let format = yuv420(BitDepth::Eight);
        let Some(mut encoder) = try_open(&video(codec, format, RESOLUTION)) else {
            continue;
        };
        let frames: Vec<_> = (0..60)
            .map(|index| yuv_frame(format, RESOLUTION, index, timestamp(index)))
            .collect();

        let high = encode_all(encoder.as_mut(), &frames[..30]);
        encoder.set_bitrate(BITRATE_KBPS / 10).unwrap();
        let low = encode_all(encoder.as_mut(), &frames[30..]);

        assert!(!low[0].keyframe);
        assert!(
            mean_size(&low[15..]) * 2 < mean_size(&high[10..]),
            "{codec}: {} bytes per frame after lowering the bitrate, {} before",
            mean_size(&low[15..]),
            mean_size(&high[10..])
        );
        assert!(matches!(
            encoder.set_bitrate(0),
            Err(EncodeError::BitrateRejected { kbps: 0, .. })
        ));
    }
}

#[test]
fn other_rate_control_modes_encode() {
    let modes = [
        RateControl::ConstantQuality { qp: 28 },
        RateControl::Vbr {
            max_bitrate_kbps: BITRATE_KBPS * 2,
        },
    ];

    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        for mode in modes {
            let format = yuv420(BitDepth::Eight);
            let config = with_vaapi(video(codec, format, RESOLUTION), |vaapi| {
                vaapi.rate_control = mode;
            });
            let Some(mut encoder) = try_open(&config) else {
                continue;
            };

            let packets = encode_all(encoder.as_mut(), &pattern_frames(format, RESOLUTION, 6));

            if let Some(pictures) = decode_vaapi(codec, &packets) {
                check_pictures(codec, &pictures, format, RESOLUTION);
            }
        }
    }
}

#[test]
fn intra_refresh_and_quality_params_encode() {
    let params = [
        [("intra-refresh-frames", "4"), ("quality-level", "1")],
        [("intra-refresh-frames", "0"), ("quality-level", "1")],
    ];

    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        for pairs in params {
            let format = yuv420(BitDepth::Eight);
            let config = with_vaapi(video(codec, format, RESOLUTION), |vaapi| {
                vaapi.params = pairs
                    .iter()
                    .map(|&(key, value)| (key.to_owned(), value.to_owned()))
                    .collect();
            });
            let Some(mut encoder) = try_open(&config) else {
                continue;
            };

            let packets = encode_all(
                encoder.as_mut(),
                &pattern_frames(format, RESOLUTION, FRAMES),
            );

            assert!(packets.iter().skip(1).all(|frame| !frame.keyframe));
            if let Some(pictures) = decode_vaapi(codec, &packets) {
                check_pictures(codec, &pictures, format, RESOLUTION);
            }
        }
    }
}

fn exported_frame(display: &Rc<Display>, format: PixelFormat, index: u64) -> (Surface, Frame) {
    let source = pattern_frame(format, RESOLUTION, index);
    let view = crate::input::YuvFrame::from_frame(&source, crate::Backend::Vaapi, format).unwrap();
    let surface_format = surface_format(format);
    let surface = Surface::create_exportable(display, surface_format, RESOLUTION).unwrap();
    let mut image = Image::new(display, surface_format.va_fourcc, RESOLUTION).unwrap();
    image
        .write(|data, layout| pack(&view, data, layout))
        .unwrap();
    image.put(&surface).unwrap();
    display.sync(surface.id()).unwrap();

    let exported = surface.export().unwrap();
    let frame = Frame {
        resolution: RESOLUTION,
        format: DrmFourcc(surface_format.drm_fourcc),
        timestamp: timestamp(index),
        color: ColorSpace::default(),
        buffer: FrameBuffer::DmaBuf {
            fd: exported.fd,
            modifier: exported.modifier,
            planes: exported
                .planes
                .iter()
                .map(|plane| Plane {
                    offset: plane.offset,
                    stride: plane.pitch,
                })
                .collect(),
        },
    };

    (surface, frame)
}

#[test]
fn dma_buf_frames_are_imported() {
    if !device_present() {
        return;
    }
    let display = Display::open(&VaapiConfig::default().device).unwrap();

    for (codec, depth) in [
        (Codec::H264, BitDepth::Eight),
        (Codec::Hevc, BitDepth::Ten),
        (Codec::Av1, BitDepth::Eight),
    ] {
        let format = yuv420(depth);
        let Some(mut encoder) = try_open(&video(codec, format, RESOLUTION)) else {
            continue;
        };

        let sources: Vec<_> = (0..3)
            .map(|index| exported_frame(&display, format, index))
            .collect();
        let mut frames: Vec<&Frame> = sources.iter().map(|(_, frame)| frame).collect();
        frames.extend(sources.iter().map(|(_, frame)| frame));
        let packets: Vec<_> = frames
            .iter()
            .map(|frame| encoder.encode(frame).unwrap().unwrap())
            .collect();

        if let Some(pictures) = decode_vaapi(codec, &packets) {
            for (index, picture) in pictures.iter().enumerate() {
                let source = &pattern_planes(format, RESOLUTION, index as u64 % 3)[0];
                let quality = psnr(source, &picture.luma, depth);

                assert_eq!(picture.format, format);
                assert!(
                    quality >= PSNR_FLOOR_DB,
                    "{codec} frame {index}: {quality:.1} dB"
                );
            }
        }
    }
}

fn fake_dma_buf(format: DrmFourcc, planes: usize) -> Frame {
    Frame {
        resolution: RESOLUTION,
        format,
        timestamp: Timestamp::ZERO,
        color: ColorSpace::default(),
        buffer: FrameBuffer::DmaBuf {
            fd: OwnedFd::from(File::open("/dev/null").unwrap()),
            modifier: 0,
            planes: vec![
                Plane {
                    offset: 0,
                    stride: RESOLUTION.width,
                };
                planes
            ],
        },
    }
}

#[test]
fn wrong_inputs_are_rejected() {
    let format = yuv420(BitDepth::Eight);
    let Some(mut encoder) = try_open(&video(Codec::Hevc, format, RESOLUTION)) else {
        return;
    };

    let wrong_layout = pattern_frame(
        pixel(ChromaSubsampling::Yuv444, BitDepth::Eight),
        RESOLUTION,
        0,
    );
    let wrong_size = pattern_frame(format, ODD_RESOLUTION, 0);
    let rgb = fake_dma_buf(DrmFourcc::from_code(*b"XR24"), 1);
    let one_plane = fake_dma_buf(DrmFourcc::from_code(*b"NV12"), 1);

    assert!(matches!(
        encoder.encode(&wrong_layout),
        Err(EncodeError::FrameFormatMismatch { expected, .. }) if expected == fourcc_for(format)
    ));
    assert!(matches!(
        encoder.encode(&wrong_size),
        Err(EncodeError::FrameResolutionMismatch { .. })
    ));
    assert!(matches!(
        encoder.encode(&rgb),
        Err(EncodeError::FrameFormatMismatch { expected, .. })
            if expected == DrmFourcc::from_code(*b"NV12")
    ));
    assert!(matches!(
        encoder.encode(&one_plane),
        Err(EncodeError::DmaBufPlaneCount {
            expected: 2,
            actual: 1,
            ..
        })
    ));
}

#[test]
fn unsupported_formats_are_reported_not_downgraded() {
    if !device_present() {
        return;
    }
    let cases = [
        (
            Codec::H264,
            pixel(ChromaSubsampling::Yuv444, BitDepth::Eight),
            "chroma",
        ),
        (Codec::H264, yuv420(BitDepth::Ten), "depth"),
        (
            Codec::Av1,
            pixel(ChromaSubsampling::Yuv422, BitDepth::Eight),
            "chroma",
        ),
        (Codec::Av1, yuv420(BitDepth::Twelve), "depth"),
    ];

    for (codec, format, kind) in cases {
        let error = open(&video(codec, format, RESOLUTION)).err().unwrap();

        match kind {
            "chroma" => assert!(matches!(
                error,
                EncodeError::ChromaUnsupported { chroma, .. } if chroma == format.chroma
            )),
            _ => assert!(matches!(
                error,
                EncodeError::BitDepthUnsupported { depth, .. } if depth == format.depth
            )),
        }
    }

    for chroma in [ChromaSubsampling::Yuv422, ChromaSubsampling::Yuv444] {
        let format = pixel(chroma, BitDepth::Eight);
        match open(&video(Codec::Hevc, format, RESOLUTION)) {
            Ok(mut encoder) => {
                let packet = encoder
                    .encode(&pattern_frame(format, RESOLUTION, 0))
                    .unwrap();
                assert!(packet.is_some_and(|frame| frame.keyframe));
            }
            Err(error) => assert!(matches!(
                error,
                EncodeError::ChromaUnsupported { chroma: reported, .. } if reported == chroma
            )),
        }
    }
}

#[test]
fn capabilities_match_what_opens() {
    if !device_present() {
        return;
    }
    let found = capabilities(&VideoEncoderConfig::Vaapi(VaapiConfig::default())).unwrap();

    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        for chroma in [
            ChromaSubsampling::Yuv420,
            ChromaSubsampling::Yuv422,
            ChromaSubsampling::Yuv444,
        ] {
            for depth in [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve] {
                let format = pixel(chroma, depth);
                let opened = open(&video(codec, format, RESOLUTION));

                assert_eq!(
                    opened.is_ok(),
                    found.supports(codec, format),
                    "{codec} {chroma} {}-bit: {:?}",
                    depth.bits(),
                    opened.err()
                );
            }
        }
    }
    assert!(
        found
            .formats(Codec::H264)
            .all(|format| format == yuv420(BitDepth::Eight))
    );
    assert!(
        found
            .formats(Codec::Av1)
            .all(|format| format.chroma != ChromaSubsampling::Yuv422)
    );
}

#[test]
fn bad_parameters_fail_at_open() {
    if !device_present() {
        return;
    }
    let format = yuv420(BitDepth::Eight);
    let config = with_vaapi(video(Codec::Hevc, format, RESOLUTION), |vaapi| {
        vaapi.params = [("no-such-knob".to_owned(), "1".to_owned())].into();
    });

    assert!(matches!(
        open(&config).err().unwrap(),
        EncodeError::UnknownParameter { ref key, .. } if key == "no-such-knob"
    ));
}

#[test]
fn resolutions_outside_the_driver_limits_are_rejected() {
    if !device_present() {
        return;
    }
    let format = yuv420(BitDepth::Eight);

    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        match open(&video(codec, format, Resolution::new(16_384, 16_384))) {
            Err(EncodeError::ResolutionUnsupported { resolution, .. }) => {
                assert_eq!(resolution, Resolution::new(16_384, 16_384));
            }
            Err(EncodeError::CodecUnsupported { .. }) => {}
            other => panic!(
                "{codec}: expected ResolutionUnsupported, got {:?}",
                other.err()
            ),
        }
    }
}

#[test]
fn unspecified_resolution_follows_the_first_frame() {
    let format = yuv420(BitDepth::Eight);
    let config = VideoConfig {
        resolution: None,
        ..video(Codec::H264, format, RESOLUTION)
    };
    let Some(mut encoder) = try_open(&config) else {
        return;
    };

    let first = encoder
        .encode(&pattern_frame(format, RESOLUTION, 0))
        .unwrap()
        .unwrap();
    let other = encoder.encode(&pattern_frame(format, ODD_RESOLUTION, 1));

    assert!(first.keyframe);
    assert!(matches!(
        other,
        Err(EncodeError::FrameResolutionMismatch { expected, .. }) if expected == RESOLUTION
    ));
}

fn info(format: PixelFormat, resolution: Resolution) -> StreamInfo {
    StreamInfo {
        resolution,
        format,
        color: ColorSpace {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            range: ColorRange::Limited,
            ..ColorSpace::default()
        },
        frame_rate: FrameRate::new(60_000, 1_001),
        peak_kbps: 80_000,
        qp: None,
    }
}

#[test]
fn h264_parameter_sets_parse_back() {
    let stream = H264Stream::new(
        H264Profile::High,
        info(yuv420(BitDepth::Eight), ODD_RESOLUTION),
    );
    let mut data = stream.sps().bytes;
    data.extend(stream.pps().bytes);
    data.extend(stream.slice_header(true).bytes);
    let mut parser = H264Parser::default();
    let mut cursor = Cursor::new(data.as_slice());

    let sps_nalu = H264Nalu::next(&mut cursor).unwrap();
    let sps = parser.parse_sps(&sps_nalu).unwrap().clone();
    let pps_nalu = H264Nalu::next(&mut cursor).unwrap();
    parser.parse_pps(&pps_nalu).unwrap();
    let rect = sps.visible_rectangle();
    let vui = &sps.vui_parameters;

    assert_eq!(sps.profile_idc, 100);
    assert_eq!(rect.max.x - rect.min.x, ODD_RESOLUTION.width);
    assert_eq!(rect.max.y - rect.min.y, ODD_RESOLUTION.height);
    assert_eq!(sps.max_num_ref_frames, 1);
    assert_eq!(vui.colour_primaries, 9);
    assert_eq!(vui.transfer_characteristics, 16);
    assert_eq!(vui.matrix_coefficients, 9);
    assert!(!vui.video_full_range_flag);
    assert_eq!(vui.max_num_reorder_frames, 0);
    assert_eq!(vui.time_scale, 120_000);
}

#[test]
fn hevc_parameter_sets_parse_back() {
    for (format, profile_idc) in [
        (yuv420(BitDepth::Ten), 2),
        (pixel(ChromaSubsampling::Yuv422, BitDepth::Ten), 4),
        (pixel(ChromaSubsampling::Yuv444, BitDepth::Eight), 4),
    ] {
        let stream = HevcStream::new(
            HevcProfile::for_format(format),
            info(format, ODD_RESOLUTION),
        );
        let mut data = stream.vps();
        data.extend(stream.sps());
        data.extend(stream.pps());
        let mut parser = H265Parser::default();
        let mut cursor = Cursor::new(data.as_slice());

        let vps = H265Nalu::next(&mut cursor).unwrap();
        parser.parse_vps(&vps).unwrap();
        let sps_nalu = H265Nalu::next(&mut cursor).unwrap();
        let sps = parser.parse_sps(&sps_nalu).unwrap().clone();
        let pps = H265Nalu::next(&mut cursor).unwrap();
        parser.parse_pps(&pps).unwrap();
        let rect = sps.visible_rectangle();

        assert_eq!(sps.profile_tier_level.general_profile_idc, profile_idc);
        assert_eq!(rect.max.x - rect.min.x, ODD_RESOLUTION.width);
        assert_eq!(rect.max.y - rect.min.y, ODD_RESOLUTION.height);
        assert_eq!(
            u32::from(sps.bit_depth_luma_minus8) + 8,
            u32::from(format.depth.bits())
        );
        assert_eq!(sps.max_num_reorder_pics[0], 0);
        assert_eq!(sps.vui_parameters.colour_primaries, 9);
    }
}

#[test]
fn av1_headers_parse_back() {
    for (format, profile) in [
        (yuv420(BitDepth::Ten), Av1Profile::Main),
        (
            pixel(ChromaSubsampling::Yuv444, BitDepth::Eight),
            Av1Profile::High,
        ),
    ] {
        let stream = Av1Stream::new(profile, info(format, ODD_RESOLUTION));
        let (frame_header, offsets) = stream.frame_header(true, 0);
        let mut data = stream.sequence_header();
        data.extend(&frame_header);

        let (has_sequence_header, types) = av1_frame_types(&data);
        let qindex_byte = offsets.qindex as usize / 8;
        let qindex_shift = offsets.qindex % 8;
        let window = u16::from_be_bytes([frame_header[qindex_byte], frame_header[qindex_byte + 1]]);

        assert!(has_sequence_header);
        assert_eq!(types, vec![FrameType::KeyFrame]);
        assert_eq!((window >> (8 - qindex_shift)) & 0xff, 128);
        assert_eq!(offsets.total_bits as usize, frame_header.len() * 8);
        assert_eq!(offsets.cdef_bits, 16);
    }
}

#[test]
fn av1_inter_header_parses_after_a_key_frame() {
    let stream = Av1Stream::new(Av1Profile::Main, info(yuv420(BitDepth::Eight), RESOLUTION));
    let mut data = stream.sequence_header();
    data.extend(stream.frame_header(true, 0).0);

    let mut parser = Av1Parser::default();
    let mut rest = data.as_slice();
    while !rest.is_empty() {
        let ObuAction::Process(obu) = parser.read_obu(rest).unwrap() else {
            panic!("dropped an obu");
        };
        rest = &rest[obu.bytes_used..];
        if let ParsedObu::FrameHeader(header) = parser.parse_obu(obu).unwrap() {
            parser.ref_frame_update(&header).unwrap();
        }
    }
    let mut inter = super::av1::temporal_delimiter();
    inter.extend(stream.frame_header(false, 1).0);
    let ObuAction::Process(delimiter) = parser.read_obu(&inter).unwrap() else {
        panic!("dropped the temporal delimiter");
    };
    let inter = &inter[delimiter.bytes_used..];
    parser.parse_obu(delimiter).unwrap();
    let ObuAction::Process(obu) = parser.read_obu(inter).unwrap() else {
        panic!("dropped the inter frame header");
    };
    let ParsedObu::FrameHeader(header) = parser.parse_obu(obu).unwrap() else {
        panic!("not a frame header");
    };

    assert_eq!(header.frame_type, FrameType::InterFrame);
    assert_eq!(header.refresh_frame_flags, 1);
    assert_eq!(header.order_hint, 1);
}

#[test]
fn av1_tiles_split_wide_frames() {
    let narrow = TileLayout::new(Resolution::new(1920, 1080));
    let wide = TileLayout::new(Resolution::new(7680, 4320));

    assert_eq!(narrow.tile_count(), 1);
    assert_eq!(wide.tile_count(), 4);
}
