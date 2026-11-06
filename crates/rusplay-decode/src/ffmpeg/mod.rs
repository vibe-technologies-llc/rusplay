mod color;

use std::{fmt, mem::ManuallyDrop, ptr, slice};

use rusplay_config::SoftwareDecoderConfig;
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution, SignalledColor,
    Timestamp,
};

use crate::{
    Backend, CpuFrame, DecodeError, DecodedBuffer, DecodedFrame, FfmpegCall, VideoDecoder,
    frame::{CpuPlane, PlaneStorage},
};

const MAX_DIMENSION: u32 = 16_384;
const MISSING_PTS: i64 = i64::MIN;
const PLANES: usize = 3;

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unsafe_op_in_unsafe_fn,
    clippy::all,
    clippy::pedantic
)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/ffmpeg.rs"));
}

pub struct FfmpegDecoder {
    codec: Codec,
    context: ManuallyDrop<Context>,
    packet: ManuallyDrop<Packet>,
    scratch: Vec<u8>,
    pending: Option<DecodedFrame>,
    last_timestamp: Timestamp,
}

impl fmt::Debug for FfmpegDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegDecoder")
            .field("codec", &self.codec)
            .finish_non_exhaustive()
    }
}

impl FfmpegDecoder {
    pub fn new(codec: Codec, config: &SoftwareDecoderConfig) -> Result<Self, DecodeError> {
        let id = codec_id(codec).ok_or(DecodeError::CodecUnsupported {
            backend: Backend::Software,
            codec,
        })?;
        let found = unsafe { sys::avcodec_find_decoder(id) };
        if found.is_null() {
            return Err(DecodeError::FfmpegDecoderMissing { codec });
        }

        let context = Context::alloc(found)?;
        unsafe {
            let context = &mut *context.0;
            context.thread_count = thread_count(config);
            context.thread_type = sys::FF_THREAD_SLICE;
            context.err_recognition = sys::RUSPLAY_AV_EF_EXPLODE;
            context.pkt_timebase = sys::AVRational {
                num: 1,
                den: 1_000_000,
            };
        }
        let status = unsafe { sys::avcodec_open2(context.0, found, ptr::null_mut()) };
        if status < 0 {
            return Err(DecodeError::Ffmpeg {
                call: FfmpegCall::Open,
                status,
            });
        }

        Ok(Self {
            codec,
            context: ManuallyDrop::new(context),
            packet: ManuallyDrop::new(Packet::alloc()?),
            scratch: Vec::new(),
            pending: None,
            last_timestamp: Timestamp::ZERO,
        })
    }

    fn submit(&mut self, data: &[u8], timestamp: Timestamp) -> Result<(), DecodeError> {
        let padding = usize::try_from(sys::AV_INPUT_BUFFER_PADDING_SIZE).unwrap_or(64);
        let padded = data
            .len()
            .checked_add(padding)
            .ok_or(DecodeError::FfmpegPacketTooLarge {
                codec: self.codec,
                bytes: data.len(),
            })?;
        let size = i32::try_from(data.len()).map_err(|_| DecodeError::FfmpegPacketTooLarge {
            codec: self.codec,
            bytes: data.len(),
        })?;

        self.scratch.clear();
        self.scratch.reserve(padded);
        self.scratch.extend_from_slice(data);
        self.scratch.resize(padded, 0);

        let pts = i64::try_from(timestamp.as_micros()).unwrap_or(MISSING_PTS);
        unsafe {
            sys::av_packet_unref(self.packet.0);
            let packet = &mut *self.packet.0;
            packet.data = self.scratch.as_mut_ptr();
            packet.size = size;
            packet.pts = pts;
        }
        self.send_loaded()
    }

    fn send_loaded(&mut self) -> Result<(), DecodeError> {
        loop {
            let status = unsafe { sys::avcodec_send_packet(self.context.0, self.packet.0) };
            if status == 0 {
                return Ok(());
            }
            if status == sys::RUSPLAY_AVERROR_EAGAIN {
                self.keep_available()?;
                continue;
            }
            return Err(DecodeError::Ffmpeg {
                call: FfmpegCall::SendPacket,
                status,
            });
        }
    }

    fn keep_available(&mut self) -> Result<(), DecodeError> {
        let fallback = self.last_timestamp;
        while let Some(picture) = self.receive(fallback)? {
            self.pending = Some(picture);
        }

        Ok(())
    }

    fn take_newest(&mut self, fallback: Timestamp) -> Result<Option<DecodedFrame>, DecodeError> {
        let mut latest = self.pending.take();
        while let Some(picture) = self.receive(fallback)? {
            latest = Some(picture);
        }

        Ok(latest)
    }

    fn receive(&mut self, fallback: Timestamp) -> Result<Option<DecodedFrame>, DecodeError> {
        let frame = OwnedFrame::alloc()?;
        let status = unsafe { sys::avcodec_receive_frame(self.context.0, frame.0) };
        if status == 0 {
            return picture(frame, self.codec, fallback).map(Some);
        }
        if status == sys::RUSPLAY_AVERROR_EAGAIN || status == sys::RUSPLAY_AVERROR_EOF {
            return Ok(None);
        }

        Err(DecodeError::Ffmpeg {
            call: FfmpegCall::ReceiveFrame,
            status,
        })
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        self.pending.take();
        unsafe {
            ManuallyDrop::drop(&mut self.packet);
            ManuallyDrop::drop(&mut self.context);
        }
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<DecodedFrame>, DecodeError> {
        if frame.codec != self.codec {
            return Err(DecodeError::CodecMismatch {
                expected: self.codec,
                found: frame.codec,
            });
        }
        if frame.data.is_empty() {
            return Ok(None);
        }

        self.submit(&frame.data, frame.timestamp)?;
        self.last_timestamp = frame.timestamp;
        self.take_newest(frame.timestamp)
    }

    fn flush(&mut self) -> Result<Vec<DecodedFrame>, DecodeError> {
        let mut frames = Vec::new();
        if let Some(pending) = self.pending.take() {
            frames.push(pending);
        }

        loop {
            let status = unsafe { sys::avcodec_send_packet(self.context.0, ptr::null()) };
            if status == 0 || status == sys::RUSPLAY_AVERROR_EOF {
                break;
            }
            if status == sys::RUSPLAY_AVERROR_EAGAIN {
                collect(self, &mut frames)?;
                continue;
            }
            return Err(DecodeError::Ffmpeg {
                call: FfmpegCall::SendPacket,
                status,
            });
        }
        collect(self, &mut frames)?;

        Ok(frames)
    }
}

fn collect(decoder: &mut FfmpegDecoder, frames: &mut Vec<DecodedFrame>) -> Result<(), DecodeError> {
    while let Some(picture) = decoder.receive(decoder.last_timestamp)? {
        frames.push(picture);
    }

    Ok(())
}

pub(crate) fn decoder_available(codec: Codec) -> bool {
    codec_id(codec).is_some_and(|id| !unsafe { sys::avcodec_find_decoder(id) }.is_null())
}

fn codec_id(codec: Codec) -> Option<sys::AVCodecID> {
    match codec {
        Codec::H264 => Some(sys::AV_CODEC_ID_H264),
        Codec::Hevc => Some(sys::AV_CODEC_ID_HEVC),
        Codec::Av1 => None,
    }
}

fn thread_count(config: &SoftwareDecoderConfig) -> i32 {
    match config.threads {
        Some(threads) => i32::try_from(threads.get()).unwrap_or(i32::MAX),
        None => 0,
    }
}

struct Context(*mut sys::AVCodecContext);

impl Context {
    fn alloc(codec: *const sys::AVCodec) -> Result<Self, DecodeError> {
        let context = unsafe { sys::avcodec_alloc_context3(codec) };
        if context.is_null() {
            Err(DecodeError::FfmpegAlloc {
                call: FfmpegCall::AllocContext,
            })
        } else {
            Ok(Self(context))
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sys::avcodec_free_context(&raw mut self.0) };
        }
    }
}

struct Packet(*mut sys::AVPacket);

impl Packet {
    fn alloc() -> Result<Self, DecodeError> {
        let packet = unsafe { sys::av_packet_alloc() };
        if packet.is_null() {
            Err(DecodeError::FfmpegAlloc {
                call: FfmpegCall::AllocPacket,
            })
        } else {
            Ok(Self(packet))
        }
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sys::av_packet_free(&raw mut self.0) };
        }
    }
}

struct OwnedFrame(*mut sys::AVFrame);

unsafe impl Send for OwnedFrame {}
unsafe impl Sync for OwnedFrame {}

impl fmt::Debug for OwnedFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("OwnedFrame").finish_non_exhaustive()
    }
}

impl OwnedFrame {
    fn alloc() -> Result<Self, DecodeError> {
        let frame = unsafe { sys::av_frame_alloc() };
        if frame.is_null() {
            Err(DecodeError::FfmpegAlloc {
                call: FfmpegCall::AllocFrame,
            })
        } else {
            Ok(Self(frame))
        }
    }

    fn frame(&self) -> &sys::AVFrame {
        unsafe { &*self.0 }
    }
}

impl Drop for OwnedFrame {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sys::av_frame_free(&raw mut self.0) };
        }
    }
}

struct SampleFormat {
    chroma: ChromaSubsampling,
    depth: BitDepth,
    jpeg_range: bool,
}

enum RejectedFormat {
    Monochrome,
    Depth(u8),
    Other,
}

fn sample_format(format: i32) -> Result<SampleFormat, RejectedFormat> {
    let eight = |chroma, jpeg_range| SampleFormat {
        chroma,
        depth: BitDepth::Eight,
        jpeg_range,
    };
    let high = |chroma, depth| SampleFormat {
        chroma,
        depth,
        jpeg_range: false,
    };

    Ok(match format {
        sys::AV_PIX_FMT_YUV420P => eight(ChromaSubsampling::Yuv420, false),
        sys::AV_PIX_FMT_YUV422P => eight(ChromaSubsampling::Yuv422, false),
        sys::AV_PIX_FMT_YUV444P => eight(ChromaSubsampling::Yuv444, false),
        sys::AV_PIX_FMT_YUVJ420P => eight(ChromaSubsampling::Yuv420, true),
        sys::AV_PIX_FMT_YUVJ422P => eight(ChromaSubsampling::Yuv422, true),
        sys::AV_PIX_FMT_YUVJ444P => eight(ChromaSubsampling::Yuv444, true),
        sys::AV_PIX_FMT_YUV420P10LE => high(ChromaSubsampling::Yuv420, BitDepth::Ten),
        sys::AV_PIX_FMT_YUV422P10LE => high(ChromaSubsampling::Yuv422, BitDepth::Ten),
        sys::AV_PIX_FMT_YUV444P10LE => high(ChromaSubsampling::Yuv444, BitDepth::Ten),
        sys::AV_PIX_FMT_YUV420P12LE => high(ChromaSubsampling::Yuv420, BitDepth::Twelve),
        sys::AV_PIX_FMT_YUV422P12LE => high(ChromaSubsampling::Yuv422, BitDepth::Twelve),
        sys::AV_PIX_FMT_YUV444P12LE => high(ChromaSubsampling::Yuv444, BitDepth::Twelve),
        sys::AV_PIX_FMT_GRAY8
        | sys::AV_PIX_FMT_GRAY9LE
        | sys::AV_PIX_FMT_GRAY10LE
        | sys::AV_PIX_FMT_GRAY12LE
        | sys::AV_PIX_FMT_GRAY14LE
        | sys::AV_PIX_FMT_GRAY16LE
        | sys::AV_PIX_FMT_GRAY8A => return Err(RejectedFormat::Monochrome),
        sys::AV_PIX_FMT_YUV420P9LE
        | sys::AV_PIX_FMT_YUV422P9LE
        | sys::AV_PIX_FMT_YUV444P9LE
        | sys::AV_PIX_FMT_YUV420P9BE
        | sys::AV_PIX_FMT_YUV422P9BE
        | sys::AV_PIX_FMT_YUV444P9BE => return Err(RejectedFormat::Depth(9)),
        sys::AV_PIX_FMT_YUV420P14LE
        | sys::AV_PIX_FMT_YUV422P14LE
        | sys::AV_PIX_FMT_YUV444P14LE
        | sys::AV_PIX_FMT_YUV420P14BE
        | sys::AV_PIX_FMT_YUV422P14BE
        | sys::AV_PIX_FMT_YUV444P14BE => return Err(RejectedFormat::Depth(14)),
        sys::AV_PIX_FMT_YUV420P16LE
        | sys::AV_PIX_FMT_YUV422P16LE
        | sys::AV_PIX_FMT_YUV444P16LE
        | sys::AV_PIX_FMT_YUV420P16BE
        | sys::AV_PIX_FMT_YUV422P16BE
        | sys::AV_PIX_FMT_YUV444P16BE => return Err(RejectedFormat::Depth(16)),
        _ => return Err(RejectedFormat::Other),
    })
}

fn picture(
    frame: OwnedFrame,
    codec: Codec,
    fallback: Timestamp,
) -> Result<DecodedFrame, DecodeError> {
    let source = frame.frame();
    if source.flags & sys::AV_FRAME_FLAG_CORRUPT != 0 {
        return Err(DecodeError::FfmpegCorruptPicture { codec });
    }

    let width = u32::try_from(source.width).unwrap_or(0);
    let height = u32::try_from(source.height).unwrap_or(0);
    if !(1..=MAX_DIMENSION).contains(&width) || !(1..=MAX_DIMENSION).contains(&height) {
        return Err(DecodeError::ResolutionUnsupported {
            backend: Backend::Software,
            codec,
            width,
            height,
        });
    }
    if source.crop_left != 0 || source.crop_top != 0 {
        return Err(DecodeError::CropOriginUnsupported {
            codec,
            left: u64::try_from(source.crop_left).unwrap_or(u64::MAX),
            top: u64::try_from(source.crop_top).unwrap_or(u64::MAX),
        });
    }

    let samples = match sample_format(source.format) {
        Ok(samples) => samples,
        Err(RejectedFormat::Monochrome) => {
            return Err(DecodeError::Monochrome {
                backend: Backend::Software,
                codec,
            });
        }
        Err(RejectedFormat::Depth(bits)) => {
            return Err(DecodeError::BitDepthUnsupported {
                backend: Backend::Software,
                codec,
                bits,
            });
        }
        Err(RejectedFormat::Other) => {
            return Err(DecodeError::FfmpegPictureFormat {
                codec,
                format: source.format,
            });
        }
    };
    let bytes_per_sample = if samples.depth == BitDepth::Eight {
        1
    } else {
        2
    };
    let resolution = Resolution::new(width, height);
    for index in 0..PLANES {
        let (plane_width, plane_height) = plane_size(samples.chroma, resolution, index);
        let linesize = source.linesize[index];
        let laid_out = plane_rows(source.data[index], linesize, plane_height)
            .is_some_and(|(_, stride)| row_fits(plane_width, bytes_per_sample, stride));
        if !laid_out {
            return Err(DecodeError::FfmpegPlaneLayout {
                codec,
                plane: plane_number(index),
                stride: linesize,
                height: plane_height,
            });
        }
    }

    let color = signalled_color(source, samples.jpeg_range);
    let timestamp = timestamp_of(source.pts, fallback);
    let format = PixelFormat {
        chroma: samples.chroma,
        depth: samples.depth,
    };

    Ok(DecodedFrame {
        resolution,
        format,
        color,
        timestamp,
        buffer: DecodedBuffer::Cpu(CpuFrame::new(PictureStorage {
            frame,
            chroma: samples.chroma,
            bytes_per_sample,
            resolution,
        })),
    })
}

fn signalled_color(frame: &sys::AVFrame, jpeg_range: bool) -> SignalledColor {
    let mut color = SignalledColor::default().with_h273_codes(
        color::h273_code(frame.color_primaries),
        color::h273_code(frame.color_trc),
        color::h273_code(frame.colorspace),
    );
    color = color::with_range(color, full_range(frame.color_range, jpeg_range));
    color = color::with_location(color, frame.chroma_location);
    color.mastering = mastering_metadata(frame);
    color.content_light = content_metadata(frame);

    color
}

fn full_range(range: sys::AVColorRange, jpeg_range: bool) -> Option<bool> {
    if jpeg_range {
        return Some(true);
    }

    match range {
        sys::AVCOL_RANGE_MPEG => Some(false),
        sys::AVCOL_RANGE_JPEG => Some(true),
        _ => None,
    }
}

fn mastering_metadata(frame: &sys::AVFrame) -> Option<rusplay_core::MasteringDisplay> {
    let metadata = side_data::<sys::AVMasteringDisplayMetadata>(
        frame,
        sys::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
    )?;
    if metadata.has_primaries == 0 || metadata.has_luminance == 0 {
        return None;
    }

    let rational = |value: sys::AVRational| color::Rational {
        num: value.num,
        den: value.den,
    };
    let pair = |value: [sys::AVRational; 2]| [rational(value[0]), rational(value[1])];

    color::mastering_from_rationals(
        pair(metadata.display_primaries[0]),
        pair(metadata.display_primaries[1]),
        pair(metadata.display_primaries[2]),
        pair(metadata.white_point),
        rational(metadata.max_luminance),
        rational(metadata.min_luminance),
    )
}

fn content_metadata(frame: &sys::AVFrame) -> Option<rusplay_core::ContentLightLevel> {
    let metadata =
        side_data::<sys::AVContentLightMetadata>(frame, sys::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL)?;

    Some(color::content_from_levels(
        metadata.MaxCLL,
        metadata.MaxFALL,
    ))
}

fn side_data<T>(frame: &sys::AVFrame, kind: sys::AVFrameSideDataType) -> Option<&T> {
    let side = unsafe { sys::av_frame_get_side_data(frame, kind) };
    if side.is_null() {
        return None;
    }

    let side = unsafe { &*side };
    if side.data.is_null() || side.size < size_of::<T>() {
        return None;
    }

    Some(unsafe { &*side.data.cast::<T>() })
}

fn timestamp_of(pts: i64, fallback: Timestamp) -> Timestamp {
    if pts == MISSING_PTS || pts < 0 {
        fallback
    } else {
        Timestamp::from_micros(u64::try_from(pts).unwrap_or(0))
    }
}

fn plane_size(chroma: ChromaSubsampling, resolution: Resolution, index: usize) -> (u32, u32) {
    if index == 0 {
        return (resolution.width, resolution.height);
    }

    match chroma {
        ChromaSubsampling::Yuv420 => (resolution.width.div_ceil(2), resolution.height.div_ceil(2)),
        ChromaSubsampling::Yuv422 => (resolution.width.div_ceil(2), resolution.height),
        ChromaSubsampling::Yuv444 => (resolution.width, resolution.height),
    }
}

fn row_fits(width: u32, bytes_per_sample: u8, stride: usize) -> bool {
    usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(usize::from(bytes_per_sample)))
        .is_some_and(|row| row > 0 && row <= stride)
}

fn plane_number(index: usize) -> u8 {
    u8::try_from(index).unwrap_or(u8::MAX)
}

fn plane_rows(data: *mut u8, linesize: i32, height: u32) -> Option<(*const u8, usize)> {
    if data.is_null() || height == 0 || linesize == 0 {
        return None;
    }

    let stride = usize::try_from(linesize.unsigned_abs()).ok()?;
    let row = stride;
    let rows = usize::try_from(height).ok()?;
    if row == 0 || stride.checked_mul(rows).is_none() {
        return None;
    }

    let origin = if linesize > 0 {
        data
    } else {
        let back = isize::try_from(height - 1).ok()?;
        let step = isize::try_from(linesize).ok()?;
        unsafe { data.offset(step.checked_mul(back)?) }
    };

    Some((origin, stride))
}

#[derive(Debug)]
struct PictureStorage {
    frame: OwnedFrame,
    chroma: ChromaSubsampling,
    bytes_per_sample: u8,
    resolution: Resolution,
}

impl PlaneStorage for PictureStorage {
    fn plane_count(&self) -> usize {
        PLANES
    }

    fn plane(&self, index: usize) -> Option<CpuPlane<'_>> {
        if index >= PLANES {
            return None;
        }

        let source = self.frame.frame();
        let (width, height) = plane_size(self.chroma, self.resolution, index);
        let (origin, stride) = plane_rows(source.data[index], source.linesize[index], height)?;
        let rows = usize::try_from(height).ok()?;
        let length = stride.checked_mul(rows)?;
        let data = unsafe { slice::from_raw_parts(origin, length) };

        Some(CpuPlane {
            data,
            stride,
            width,
            height,
            bytes_per_sample: self.bytes_per_sample,
        })
    }
}
