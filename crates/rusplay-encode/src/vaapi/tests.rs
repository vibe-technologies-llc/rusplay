mod compare;
mod measure;

use std::{
    fs::File,
    io::{Cursor, Write},
    os::fd::{AsFd, OwnedFd},
    path::Path,
    process::{Command, Stdio},
    rc::Rc,
    thread,
};

use cros_codecs::codec::{
    h264::parser::{Nalu as H264Nalu, NaluType as H264NaluType},
    h265::parser::{Nalu as H265Nalu, NaluType as H265NaluType},
};
use rusplay_config::{
    RateControl, SoftwareDecoderConfig, VaapiConfig, VaapiDecoderConfig, VideoConfig,
    VideoEncoderConfig,
};
use rusplay_core::{
    BitDepth, BufferLease, ChromaSubsampling, Codec, ColorSpace, DrmFourcc, EncodedFrame, Frame,
    FrameBuffer, PixelFormat, Plane, Resolution, Timestamp,
};
use rusplay_decode::{DecodedBuffer, DecodedFrame, Rav1dDecoder, VaapiDecoder, VideoDecoder};
use rusplay_va::ffi;

use crate::{
    EncodeError, OutputIndex, Recovery, VideoEncoder, capabilities, hardware,
    hdr::fixtures::{hdr10_color, signalled},
    input::{bytes_per_sample, fixtures::yuv_frame, fourcc_for},
    open,
    vaapi::{
        IntraRefresh, RefreshDirection,
        format::{pack, surface_format},
        long_term::driver_honours_long_term,
        refresh::{RefreshCadence, RefreshRegion},
        refresh_parameters,
        va::{
            Display, DmaBufImport, Image, Surface, export_surface, exportable_surface,
            imported_surface,
        },
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
        codec: rusplay_config::CodecChoice::pinned(codec),
        pixel_format: rusplay_config::PixelFormatChoice::pinned(format),
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
    hardware::device_present(Path::new(&VaapiConfig::default().device))
}

fn try_open(config: &VideoConfig) -> Option<Box<dyn VideoEncoder>> {
    if !device_present() {
        return None;
    }

    match open(config) {
        Ok(encoder) => Some(encoder),
        Err(
            error @ (EncodeError::CodecUnsupported { .. }
            | EncodeError::ChromaUnsupported { .. }
            | EncodeError::BitDepthUnsupported { .. }
            | EncodeError::ResolutionUnsupported { .. }),
        ) => {
            hardware::unsupported(&error);
            None
        }
        Err(error) => panic!(
            "opening the {} vaapi encoder: {error}",
            config.stream_codec()
        ),
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

pub(crate) fn pattern_planes(
    format: PixelFormat,
    resolution: Resolution,
    index: u64,
) -> Vec<Vec<u16>> {
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

pub(crate) fn encode_all(encoder: &mut dyn VideoEncoder, frames: &[Frame]) -> Vec<EncodedFrame> {
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

pub(crate) fn pattern_frames(
    format: PixelFormat,
    resolution: Resolution,
    count: u64,
) -> Vec<Frame> {
    (0..count)
        .map(|index| pattern_frame(format, resolution, index))
        .collect()
}

#[derive(Debug)]
pub(crate) struct Picture {
    pub(crate) resolution: Resolution,
    pub(crate) format: PixelFormat,
    pub(crate) luma: Vec<u16>,
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
    let surface = imported_surface(
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
    let layout = va_image.layout();

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

pub(crate) fn decode_vaapi(codec: Codec, frames: &[EncodedFrame]) -> Option<Vec<Picture>> {
    let mut decoder = match VaapiDecoder::new(codec, &VaapiDecoderConfig::default()) {
        Ok(decoder) => decoder,
        Err(error @ rusplay_decode::DecodeError::CodecUnsupported { .. }) => {
            hardware::unsupported(&error);
            return None;
        }
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

fn decode_rav1d<'a>(frames: impl IntoIterator<Item = &'a EncodedFrame>) -> Vec<Picture> {
    let mut decoder = Rav1dDecoder::new(&SoftwareDecoderConfig::default()).unwrap();

    frames
        .into_iter()
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

pub(crate) fn psnr(source: &[u16], output: &[u16], depth: BitDepth) -> f64 {
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

        match VaapiDecoder::new(codec, &VaapiDecoderConfig::default()) {
            Ok(mut decoder) => {
                for packet in &packets {
                    let output = decoder.decode(packet).unwrap().unwrap();
                    assert_eq!(output.color, expected, "{codec} through vaapi");
                }
            }
            Err(error @ rusplay_decode::DecodeError::CodecUnsupported { .. }) => {
                hardware::unsupported(&error);
            }
            Err(error) => panic!("opening the {codec} vaapi decoder: {error}"),
        }
        if codec == Codec::Av1 {
            let mut decoder = Rav1dDecoder::new(&SoftwareDecoderConfig::default()).unwrap();
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

fn pick(packets: &[EncodedFrame], received: &[u64]) -> Vec<EncodedFrame> {
    received
        .iter()
        .map(|&index| packets[usize::try_from(index).unwrap()].clone())
        .collect()
}

fn lumas(pictures: Vec<Picture>) -> Vec<Vec<u16>> {
    pictures.into_iter().map(|picture| picture.luma).collect()
}

struct Ffmpeg {
    lumas: Vec<Vec<u16>>,
    errors: String,
}

fn decode_ffmpeg(
    codec: Codec,
    format: PixelFormat,
    resolution: Resolution,
    frames: &[EncodedFrame],
) -> Option<Ffmpeg> {
    let demuxer = if codec == Codec::H264 { "h264" } else { "hevc" };
    let (planar, sample) = if format.depth == BitDepth::Eight {
        ("yuv420p", 1)
    } else {
        ("yuv420p10le", 2)
    };
    let child = Command::new("ffmpeg")
        .args(["-hide_banner", "-v", "error", "-f", demuxer, "-i", "pipe:0"])
        .args(["-fps_mode", "passthrough", "-pix_fmt", planar])
        .args(["-f", "rawvideo", "pipe:1"])
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

    let stream: Vec<u8> = frames
        .iter()
        .flat_map(|frame| frame.data.iter().copied())
        .collect();
    let mut input = child.stdin.take().unwrap();
    let writer = thread::spawn(move || {
        let _ = input.write_all(&stream);
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();

    let luma_size = usize::try_from(resolution.pixel_count()).unwrap() * sample;
    let lumas = output
        .stdout
        .chunks_exact(luma_size * 3 / 2)
        .map(|frame| {
            frame[..luma_size]
                .chunks_exact(sample)
                .map(|bytes| {
                    if sample == 2 {
                        u16::from_le_bytes([bytes[0], bytes[1]])
                    } else {
                        u16::from(bytes[0])
                    }
                })
                .collect()
        })
        .collect();

    Some(Ffmpeg {
        lumas,
        errors: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn check_received(
    codec: Codec,
    received: &[u64],
    pictures: &[Vec<u16>],
    format: PixelFormat,
    through: &str,
) {
    assert_eq!(pictures.len(), received.len(), "{codec} through {through}");
    for (index, luma) in received.iter().zip(pictures) {
        let source = &pattern_planes(format, RESOLUTION, *index)[0];
        let quality = psnr(source, luma, format.depth);

        assert!(
            quality >= PSNR_FLOOR_DB,
            "{codec} frame {index} through {through} decodes at {quality:.1} dB"
        );
    }
}

fn check_every_decoder(
    codec: Codec,
    format: PixelFormat,
    received: &[u64],
    packets: &[EncodedFrame],
) {
    let survivors = pick(packets, received);

    if codec == Codec::Av1 {
        let decoded = lumas(decode_rav1d(&survivors));
        check_received(codec, received, &decoded, format, "rav1d");
        return;
    }
    if let Some(pictures) = decode_vaapi(codec, &survivors) {
        check_received(codec, received, &lumas(pictures), format, "vaapi");
    }
    if let Some(ffmpeg) = decode_ffmpeg(codec, format, RESOLUTION, &survivors) {
        assert_eq!(ffmpeg.errors, "", "{codec} through ffmpeg");
        check_received(codec, received, &ffmpeg.lumas, format, "ffmpeg");
    }
}

fn long_term_verified() -> bool {
    let vendor = Display::open(&VaapiConfig::default().device)
        .ok()
        .and_then(|display| display.vendor())
        .unwrap_or_default();
    let verified = driver_honours_long_term(&vendor);
    if !verified {
        hardware::unsupported(format_args!(
            "long-term reference recovery is unverified on {vendor}"
        ));
    }

    verified
}

fn without_intra_refresh(config: VideoConfig) -> VideoConfig {
    with_vaapi(config, |vaapi| {
        vaapi.params = [("intra-refresh-frames".to_owned(), "0".to_owned())].into();
    })
}

struct LossCase {
    codec: Codec,
    format: PixelFormat,
    acknowledged: Option<u64>,
    last_good: u64,
    resumed: u64,
    count: u64,
    reference: Option<u64>,
}

fn recover_after_a_loss(case: &LossCase, config: &VideoConfig) {
    let format = case.format;
    let codec = case.codec;
    let Some(mut encoder) = try_open(config) else {
        return;
    };
    let frames = pattern_frames(format, RESOLUTION, case.count);
    let split = usize::try_from(case.resumed).unwrap();

    let mut packets = encode_all(encoder.as_mut(), &frames[..split]);
    if let Some(output) = case.acknowledged {
        encoder.acknowledge(OutputIndex(output));
    }
    let recovery = encoder.recover(OutputIndex(case.last_good));
    packets.extend(encode_all(encoder.as_mut(), &frames[split..]));
    let received: Vec<u64> = (0..=case.last_good)
        .chain(case.resumed..case.count)
        .collect();

    let Recovery::Reference(OutputIndex(reference)) = recovery else {
        panic!("vaapi {codec} fell back to a keyframe: {recovery:?}");
    };
    assert!(reference <= case.last_good);
    if let Some(expected) = case.reference {
        assert_eq!(reference, expected, "{codec}");
    }
    assert!(case.last_good - reference < 16);
    assert!(!packets[split].keyframe);
    assert!(!keyframe_check(codec)(&packets[split].data));
    assert!(packets[split + 1..].iter().all(|frame| !frame.keyframe));
    check_every_decoder(codec, format, &received, &packets);
}

fn av1_case(acknowledged: Option<u64>) -> LossCase {
    LossCase {
        codec: Codec::Av1,
        format: yuv420(BitDepth::Eight),
        acknowledged,
        last_good: 14,
        resumed: 20,
        count: 30,
        reference: None,
    }
}

#[test]
fn av1_recovers_against_an_acknowledged_reference_without_a_keyframe() {
    let case = av1_case(Some(12));

    recover_after_a_loss(&case, &video(case.codec, case.format, RESOLUTION));
}

#[test]
fn av1_recovers_against_the_last_good_frame_without_acknowledgements() {
    let case = av1_case(None);

    recover_after_a_loss(&case, &video(case.codec, case.format, RESOLUTION));
}

fn h26x_cases() -> Vec<(Codec, PixelFormat)> {
    vec![
        (Codec::H264, yuv420(BitDepth::Eight)),
        (Codec::Hevc, yuv420(BitDepth::Eight)),
        (Codec::Hevc, yuv420(BitDepth::Ten)),
    ]
}

#[test]
fn h26x_recovers_against_the_long_term_frame_without_a_keyframe() {
    if !device_present() || !long_term_verified() {
        return;
    }

    for (codec, format) in h26x_cases() {
        for acknowledged in [Some(16), None] {
            let case = LossCase {
                codec,
                format,
                acknowledged,
                last_good: 18,
                resumed: 23,
                count: 32,
                reference: Some(17),
            };
            let config = video(codec, format, RESOLUTION);

            recover_after_a_loss(&case, &config);
            recover_after_a_loss(&case, &without_intra_refresh(config));
        }
    }
}

#[test]
fn h26x_recovers_from_the_candidate_right_after_marking_it() {
    if !device_present() || !long_term_verified() {
        return;
    }

    for (codec, format) in h26x_cases() {
        let case = LossCase {
            codec,
            format,
            acknowledged: None,
            last_good: 17,
            resumed: 21,
            count: 30,
            reference: Some(17),
        };

        recover_after_a_loss(
            &case,
            &without_intra_refresh(video(codec, format, RESOLUTION)),
        );
    }
}

#[test]
fn h26x_recovers_from_the_older_slot_while_the_newest_candidate_is_unacknowledged() {
    if !device_present() || !long_term_verified() {
        return;
    }

    for (codec, format) in h26x_cases() {
        for acknowledged in [Some(10), None] {
            let case = LossCase {
                codec,
                format,
                acknowledged,
                last_good: 16,
                resumed: 20,
                count: 30,
                reference: Some(9),
            };
            let config = video(codec, format, RESOLUTION);

            recover_after_a_loss(&case, &config);
            recover_after_a_loss(&case, &without_intra_refresh(config));
        }
    }
}

#[test]
fn h26x_without_a_recovery_frame_is_visibly_wrong_after_the_loss() {
    const LAST_GOOD: u64 = 18;
    const RESUMED: u64 = 23;
    const COUNT: u64 = 32;
    const CORRUPT_CEILING_DB: f64 = 30.0;

    if !device_present() || !long_term_verified() {
        return;
    }

    for (codec, format) in h26x_cases() {
        let config = without_intra_refresh(video(codec, format, RESOLUTION));
        let Some(mut encoder) = try_open(&config) else {
            continue;
        };
        let packets = encode_all(encoder.as_mut(), &pattern_frames(format, RESOLUTION, COUNT));
        let received: Vec<u64> = (0..=LAST_GOOD).chain(RESUMED..COUNT).collect();
        let survivors = pick(&packets, &received);
        if codec == Codec::Hevc {
            assert_refused_after_the_loss(&survivors, LAST_GOOD);
            continue;
        }
        let Some(pictures) = decode_vaapi(codec, &survivors) else {
            continue;
        };

        let source = &pattern_planes(format, RESOLUTION, RESUMED)[0];
        let at_resume = usize::try_from(LAST_GOOD + 1).unwrap();
        let quality = psnr(source, &pictures[at_resume].luma, format.depth);

        assert!(
            quality < CORRUPT_CEILING_DB,
            "{codec} decodes the frame after the loss at {quality:.1} dB without a recovery frame"
        );
    }
}

fn assert_refused_after_the_loss(survivors: &[EncodedFrame], last_good: u64) {
    let mut decoder = VaapiDecoder::new(Codec::Hevc, &VaapiDecoderConfig::default()).unwrap();
    let refused = survivors
        .iter()
        .enumerate()
        .find_map(|(index, frame)| decoder.decode(frame).err().map(|error| (index, error)));
    let resumed = usize::try_from(last_good + 1).unwrap();

    assert!(
        matches!(
            refused,
            Some((
                index,
                rusplay_decode::DecodeError::ReferenceMissing {
                    codec: Codec::Hevc,
                    ..
                }
            )) if index == resumed
        ),
        "{refused:?}"
    );
}

struct LossEvent {
    first_lost: u64,
    resumed: u64,
}

struct LossRun<'a> {
    count: u64,
    events: &'a [LossEvent],
    acknowledged_until: u64,
}

const ACK_DELAY: u64 = 2;

const REPEATED_LOSSES: LossRun<'static> = LossRun {
    count: 90,
    events: &[
        LossEvent {
            first_lost: 19,
            resumed: 23,
        },
        LossEvent {
            first_lost: 24,
            resumed: 26,
        },
        LossEvent {
            first_lost: 35,
            resumed: 37,
        },
        LossEvent {
            first_lost: 42,
            resumed: 43,
        },
        LossEvent {
            first_lost: 52,
            resumed: 53,
        },
        LossEvent {
            first_lost: 60,
            resumed: 61,
        },
        LossEvent {
            first_lost: 72,
            resumed: 75,
        },
    ],
    acknowledged_until: u64::MAX,
};

const LOSSES_PAST_THE_ORDER_COUNT_RANGE: LossRun<'static> = LossRun {
    count: 1100,
    events: &[
        LossEvent {
            first_lost: 268,
            resumed: 272,
        },
        LossEvent {
            first_lost: 520,
            resumed: 523,
        },
        LossEvent {
            first_lost: 1050,
            resumed: 1053,
        },
    ],
    acknowledged_until: 600,
};

fn survive_losses(codec: Codec, format: PixelFormat, config: &VideoConfig, run: &LossRun) {
    let Some(mut encoder) = try_open(config) else {
        return;
    };
    let mut packets = Vec::new();
    let mut received = Vec::new();

    for index in 0..run.count {
        let lost = run
            .events
            .iter()
            .any(|event| (event.first_lost..event.resumed).contains(&index));
        let acknowledged = received
            .iter()
            .rev()
            .find(|&&frame| frame + ACK_DELAY <= index)
            .copied()
            .filter(|_| index < run.acknowledged_until);
        if let Some(frame) = acknowledged {
            encoder.acknowledge(OutputIndex(frame));
        }
        if let Some(event) = run.events.iter().find(|event| event.resumed == index) {
            let last_good = event.first_lost - 1;
            let recovery = encoder.recover(OutputIndex(last_good));

            assert!(
                matches!(recovery, Recovery::Reference(_)),
                "{codec} at frame {index}: {recovery:?}"
            );
            if let Recovery::Reference(OutputIndex(reference)) = recovery {
                assert!(reference <= last_good, "{codec} at frame {index}");
            }
        }

        let frame = pattern_frame(format, RESOLUTION, index);
        let packet = encoder.encode(&frame).unwrap().unwrap();
        if run.events.iter().any(|event| event.resumed == index) {
            assert!(!packet.keyframe, "{codec} at frame {index}");
        }
        packets.push(packet);
        if !lost {
            received.push(index);
        }
    }

    check_every_decoder(codec, format, &received, &packets);
}

#[test]
fn h26x_survives_repeated_losses_with_acknowledgements() {
    if !device_present() || !long_term_verified() {
        return;
    }

    for (codec, format) in h26x_cases() {
        let config = video(codec, format, RESOLUTION);

        survive_losses(codec, format, &config, &REPEATED_LOSSES);
        survive_losses(
            codec,
            format,
            &without_intra_refresh(config),
            &REPEATED_LOSSES,
        );
    }
}

#[test]
fn h26x_long_term_references_survive_past_the_order_count_lsb_range() {
    if !device_present() || !long_term_verified() {
        return;
    }

    for (codec, format) in h26x_cases() {
        let config = without_intra_refresh(video(codec, format, RESOLUTION));

        survive_losses(codec, format, &config, &LOSSES_PAST_THE_ORDER_COUNT_RANGE);
    }
}

#[test]
fn recovery_before_any_frame_is_a_keyframe() {
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let format = yuv420(BitDepth::Eight);
        let Some(mut encoder) = try_open(&video(codec, format, RESOLUTION)) else {
            continue;
        };

        let early = encoder.recover(OutputIndex(0));
        let first = encoder
            .encode(&pattern_frame(format, RESOLUTION, 0))
            .unwrap()
            .unwrap();
        for index in 1..4 {
            encoder
                .encode(&pattern_frame(format, RESOLUTION, index))
                .unwrap();
        }
        let later = encoder.recover(OutputIndex(2));
        let answered = encoder
            .encode(&pattern_frame(format, RESOLUTION, 4))
            .unwrap()
            .unwrap();

        assert_eq!(early, Recovery::Keyframe, "{codec}");
        assert!(first.keyframe);
        if codec == Codec::Av1 || long_term_verified() {
            assert!(matches!(later, Recovery::Reference(OutputIndex(0..=2))));
            assert!(!answered.keyframe);
        } else {
            assert_eq!(later, Recovery::Keyframe, "{codec}");
            assert!(answered.keyframe, "{codec}");
        }
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

#[test]
fn a_frame_without_a_region_switches_intra_refresh_off() {
    let refresh = IntraRefresh {
        direction: RefreshDirection::Rows,
        period: 60,
    };
    let region = RefreshRegion {
        location: 5,
        size: 2,
    };

    let refreshing = refresh_parameters(refresh, Some(region));
    let idle = refresh_parameters(refresh, None);

    assert_eq!(
        unsafe { refreshing.rir_flags.value },
        ffi::VA_ENC_INTRA_REFRESH_ROLLING_ROW
    );
    assert_eq!(refreshing.intra_insertion_location, 5);
    assert_eq!(refreshing.intra_insert_size, 2);
    assert_eq!(unsafe { idle.rir_flags.value }, 0);
    assert_eq!(idle.intra_insertion_location, 0);
    assert_eq!(idle.intra_insert_size, 0);
}

fn mean_bytes(sizes: &[usize]) -> usize {
    sizes.iter().sum::<usize>() / sizes.len().max(1)
}

fn refresh_rows(codec: Codec) -> u32 {
    let block = if codec == Codec::H264 { 16 } else { 64 };

    RESOLUTION.height.div_ceil(block)
}

fn refresh_config(codec: Codec, period: u32, rate_control: RateControl) -> VideoConfig {
    with_vaapi(video(codec, yuv420(BitDepth::Eight), RESOLUTION), |vaapi| {
        vaapi.rate_control = rate_control;
        vaapi.params = [("intra-refresh-frames".to_owned(), period.to_string())].into();
    })
}

#[test]
fn intra_refresh_spends_its_rows_over_the_whole_period() {
    const SWEEPS: u32 = 2;
    const REFRESH_TO_IDLE_BYTES: usize = 4;

    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let rows = refresh_rows(codec);
        let period = rows * 3;
        let config = refresh_config(codec, period, RateControl::ConstantQuality { qp: 26 });
        let Some(mut encoder) = try_open(&config) else {
            continue;
        };
        let still = yuv_frame(yuv420(BitDepth::Eight), RESOLUTION, 0, timestamp(0));
        let mut cadence = RefreshCadence::new(rows, period);

        let sizes: Vec<usize> = (0..=period * SWEEPS)
            .map(|_| encoder.encode(&still).unwrap().unwrap().data.len())
            .collect();
        let refreshes: Vec<bool> = (0..sizes.len())
            .map(|frame| frame > 0 && cadence.advance().is_some())
            .collect();
        let sizes_where = |refreshing: bool| -> Vec<usize> {
            (1..sizes.len())
                .filter(|&frame| refreshes[frame] == refreshing)
                .map(|frame| sizes[frame])
                .collect()
        };
        let (refreshing, idle) = (sizes_where(true), sizes_where(false));
        let outweighs_its_neighbours = |frame: usize| {
            sizes[frame] > sizes[frame - 1]
                && sizes.get(frame + 1).is_none_or(|&next| sizes[frame] > next)
        };

        assert_eq!(refreshing.len(), (rows * SWEEPS) as usize, "{codec}");
        assert!(
            (1..sizes.len())
                .filter(|&frame| refreshes[frame])
                .all(outweighs_its_neighbours),
            "{codec}: {sizes:?}"
        );
        assert!(
            mean_bytes(&refreshing) > mean_bytes(&idle) * REFRESH_TO_IDLE_BYTES,
            "{codec}: frames with an intra region average {} bytes, the others {}",
            mean_bytes(&refreshing),
            mean_bytes(&idle)
        );
    }
}

fn likeness_after_a_lost_h264_frame(period: u32, lost: u64, count: u64) -> Option<Vec<f64>> {
    const SCENE_CHANGE: u64 = 40;

    let codec = Codec::H264;
    let format = yuv420(BitDepth::Eight);
    let mut encoder = try_open(&refresh_config(codec, period, RateControl::default()))?;
    let frames: Vec<Frame> = (0..count)
        .map(|index| {
            let scene = if index < lost { 0 } else { SCENE_CHANGE };
            yuv_frame(format, RESOLUTION, scene, timestamp(index))
        })
        .collect();

    let packets = encode_all(encoder.as_mut(), &frames);
    let received: Vec<u64> = (0..count).filter(|&index| index != lost).collect();
    let whole = decode_ffmpeg(codec, format, RESOLUTION, &packets)?;
    let broken = decode_ffmpeg(codec, format, RESOLUTION, &pick(&packets, &received))?;

    assert_eq!(whole.errors, "");
    assert_eq!(whole.lumas.len(), packets.len());
    assert_eq!(broken.lumas.len(), received.len());

    Some(
        (lost + 1..count)
            .map(|index| {
                let index = usize::try_from(index).unwrap();
                psnr(&whole.lumas[index], &broken.lumas[index - 1], format.depth)
            })
            .collect(),
    )
}

fn assert_healed_from(likeness: &[f64], lost: u64, healed: u64) {
    const CORRUPT_CEILING_DB: f64 = 30.0;
    const HEALED_FLOOR_DB: f64 = 50.0;

    let after_loss = usize::try_from(healed - lost - 1).unwrap();

    assert!(
        likeness[0] < CORRUPT_CEILING_DB,
        "the frame after the loss is {:.1} dB from the unbroken stream",
        likeness[0]
    );
    for (offset, quality) in likeness.iter().enumerate().skip(after_loss) {
        assert!(
            *quality >= HEALED_FLOOR_DB,
            "frame {} is {quality:.1} dB from the unbroken stream, the sweep ended at frame {healed}",
            lost + 1 + offset as u64
        );
    }
}

#[test]
fn an_h264_frame_lost_before_a_sweep_heals_by_the_end_of_that_sweep() {
    let period = refresh_rows(Codec::H264) * 3;
    let sweep = u64::from(period);
    let lost = sweep + 1;
    let healed = 2 * sweep;

    let Some(likeness) = likeness_after_a_lost_h264_frame(period, lost, healed + sweep) else {
        return;
    };

    assert_healed_from(&likeness, lost, healed);
}

#[test]
fn an_h264_frame_lost_in_mid_sweep_heals_by_the_end_of_the_next_sweep() {
    let period = refresh_rows(Codec::H264) * 3;
    let sweep = u64::from(period);
    let lost = sweep + sweep / 2;
    let healed = 3 * sweep;

    let Some(likeness) = likeness_after_a_lost_h264_frame(period, lost, healed + sweep) else {
        return;
    };

    assert_healed_from(&likeness, lost, healed);
}

fn exported_frame(display: &Rc<Display>, format: PixelFormat, index: u64) -> (Surface, Frame) {
    let source = pattern_frame(format, RESOLUTION, index);
    let view = crate::input::YuvFrame::from_frame(&source, crate::Backend::Vaapi, format).unwrap();
    let surface_format = surface_format(format);
    let surface = exportable_surface(display, surface_format, RESOLUTION).unwrap();
    let mut image = Image::new(display, surface_format.va_fourcc, RESOLUTION).unwrap();
    image
        .write(|data, layout| pack(&view, data, layout, RESOLUTION))
        .unwrap();
    image.put(&surface).unwrap();
    display.sync(surface.id()).unwrap();

    let exported = export_surface(&surface).unwrap();
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
            lease: BufferLease::detached(),
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
            lease: BufferLease::detached(),
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

#[test]
fn reconstruction_is_what_the_decoder_outputs() {
    const MEASURED: usize = 6;

    for (codec, format) in [
        (Codec::H264, yuv420(BitDepth::Eight)),
        (Codec::Hevc, yuv420(BitDepth::Eight)),
        (Codec::Hevc, yuv420(BitDepth::Ten)),
        (Codec::Av1, yuv420(BitDepth::Eight)),
        (Codec::Av1, yuv420(BitDepth::Ten)),
    ] {
        for resolution in [RESOLUTION, ODD_RESOLUTION] {
            let config = video(codec, format, resolution);
            let Some(mut encoder) = try_open(&config) else {
                continue;
            };
            let frames = pattern_frames(format, resolution, FRAMES);

            let mut packets = encode_all(&mut *encoder, &frames[..MEASURED]);
            assert!(encoder.capture_reconstruction());
            packets.push(encoder.encode(&frames[MEASURED]).unwrap().unwrap());
            let reconstruction = encoder.take_reconstruction().unwrap().unwrap();
            let Some(pictures) = decode_vaapi(codec, &packets) else {
                continue;
            };

            assert_eq!(reconstruction.resolution(), resolution);
            assert_eq!(
                reconstruction.luma().samples(),
                pictures[MEASURED].luma.as_slice(),
                "{codec} {format:?} at {resolution}: the reconstruction differs from the decoded frame"
            );
        }
    }
}
