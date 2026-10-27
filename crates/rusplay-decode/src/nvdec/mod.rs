mod color;
mod driver;
mod ffi;
mod format;
mod host;

use std::{
    ffi::{c_int, c_uint, c_ulong, c_void},
    fmt, mem,
    ptr::{self, NonNull},
    rc::Rc,
};

use rusplay_config::NvdecDecoderConfig;
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, CodecFormat, EncodedFrame, FormatSet, PixelFormat,
    SignalledColor, Timestamp,
};

use crate::{
    Backend, CudaCall, DecodeError, DecodedBuffer, DecodedFrame, VideoDecoder,
    nvdec::{
        color::ColorReader,
        driver::{Context, Driver, check},
        ffi::{
            CuDevicePtr, CuVideoDecoder, CuVideoParser, DecodeCaps, DecodeCreateInfo, Memcpy2D,
            ParserDisplayInfo, ParserParams, PicParams, ProcParams, ShortRect, SourceDataPacket,
            VideoFormat,
        },
        format::{StreamFormat, cuda_chroma, cuda_codec, decodes},
        host::{BufferPool, HostFrame, HostLayout, PooledBuffer, deinterleave, shift_to_low_bits},
    },
};

const MICROSECOND_CLOCK: c_uint = 1_000_000;
const NO_DISPLAY_DELAY: c_uint = 0;
const SKIP_CORRUPTED_PICTURES: c_uint = 0;
const PARSER_INITIAL_SURFACES: c_uint = 1;
const MAPPED_OUTPUT_SURFACES: c_ulong = 1;
const CALLBACK_FAILED: c_int = 0;
const CALLBACK_SUCCEEDED: c_int = 1;

pub struct NvdecDecoder {
    codec: Codec,
    color: ColorReader,
    parser: Parser,
    context: Rc<Context>,
}

impl fmt::Debug for NvdecDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NvdecDecoder")
            .field("codec", &self.codec)
            .finish_non_exhaustive()
    }
}

impl NvdecDecoder {
    pub fn new(codec: Codec, config: &NvdecDecoderConfig) -> Result<Self, DecodeError> {
        let driver = Driver::load()?;
        let context = Context::open(&driver, config.gpu_index)?;

        {
            let _current = context.make_current()?;
            let base = PixelFormat {
                chroma: ChromaSubsampling::Yuv420,
                depth: BitDepth::Eight,
            };
            if decoder_caps(&driver, codec, base)?.is_supported == 0 {
                return Err(DecodeError::CodecUnsupported {
                    backend: Backend::Nvdec,
                    codec,
                });
            }
        }

        let parser = Parser::new(&context, codec)?;

        Ok(Self {
            codec,
            color: ColorReader::new(codec),
            parser,
            context,
        })
    }
}

impl VideoDecoder for NvdecDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<DecodedFrame>, DecodeError> {
        if frame.codec != self.codec {
            return Err(DecodeError::CodecMismatch {
                expected: self.codec,
                found: frame.codec,
            });
        }

        self.color.read(&frame.data)?;
        let _current = self.context.make_current()?;
        self.parser.state().color = self.color.current();

        let mut packet = SourceDataPacket {
            flags: ffi::PACKET_TIMESTAMP | ffi::PACKET_END_OF_PICTURE,
            payload_size: c_ulong::try_from(frame.data.len()).unwrap_or(c_ulong::MAX),
            payload: frame.data.as_ptr(),
            timestamp: i64::try_from(frame.timestamp.as_micros()).unwrap_or(i64::MAX),
        };
        self.parser.parse(&mut packet)?;

        self.parser.state().newest()
    }

    fn flush(&mut self) -> Result<Vec<DecodedFrame>, DecodeError> {
        let _current = self.context.make_current()?;

        let mut packet = SourceDataPacket {
            flags: ffi::PACKET_END_OF_STREAM,
            payload_size: 0,
            payload: ptr::null(),
            timestamp: 0,
        };
        self.parser.parse(&mut packet)?;

        self.parser.state().all()
    }
}

pub(crate) fn capabilities(config: &NvdecDecoderConfig) -> Result<FormatSet, DecodeError> {
    let driver = Driver::load()?;
    let context = Context::open(&driver, config.gpu_index)?;
    let _current = context.make_current()?;
    let mut formats = FormatSet::EMPTY;

    for format in CodecFormat::all() {
        if decodes(
            &decoder_caps(&driver, format.codec, format.format)?,
            format.format,
        ) {
            formats.insert(format);
        }
    }

    Ok(formats)
}

fn decoder_caps(
    driver: &Driver,
    codec: Codec,
    format: PixelFormat,
) -> Result<DecodeCaps, DecodeError> {
    let mut caps = DecodeCaps {
        codec: cuda_codec(codec),
        chroma_format: cuda_chroma(format.chroma),
        bit_depth_minus8: c_uint::from(format.depth.bits() - 8),
        ..DecodeCaps::default()
    };
    check(CudaCall::GetDecoderCaps, unsafe {
        (driver.cuvid.get_decoder_caps)(&raw mut caps)
    })?;

    Ok(caps)
}

struct Parser {
    driver: Rc<Driver>,
    handle: CuVideoParser,
    state: NonNull<ParserState>,
}

impl Parser {
    fn new(context: &Rc<Context>, codec: Codec) -> Result<Self, DecodeError> {
        let driver = Rc::clone(context.driver());
        let state = NonNull::from(Box::leak(Box::new(ParserState {
            context: Rc::clone(context),
            codec,
            decoder: None,
            pool: BufferPool::new(),
            color: SignalledColor::default(),
            pending: Vec::new(),
            ready: Vec::new(),
            error: None,
        })));
        let mut parser = Self {
            driver,
            handle: ptr::null_mut(),
            state,
        };

        let mut params = ParserParams {
            codec: cuda_codec(codec),
            max_num_decode_surfaces: PARSER_INITIAL_SURFACES,
            clock_rate: MICROSECOND_CLOCK,
            error_threshold: SKIP_CORRUPTED_PICTURES,
            max_display_delay: NO_DISPLAY_DELAY,
            flags: 0,
            reserved1: [0; 4],
            user_data: state.as_ptr().cast(),
            sequence_callback: Some(on_sequence),
            decode_picture: Some(on_decode),
            display_picture: Some(on_display),
            get_operating_point: None,
            get_sei_message: None,
            reserved2: [ptr::null_mut(); 5],
            ext_video_info: ptr::null_mut(),
        };
        check(CudaCall::CreateVideoParser, unsafe {
            (parser.driver.cuvid.create_video_parser)(&raw mut parser.handle, &raw mut params)
        })?;

        Ok(parser)
    }

    fn state(&mut self) -> &mut ParserState {
        unsafe { self.state.as_mut() }
    }

    fn parse(&mut self, packet: &mut SourceDataPacket) -> Result<(), DecodeError> {
        let result = unsafe { (self.driver.cuvid.parse_video_data)(self.handle, packet) };

        if let Some(error) = self.state().error.take() {
            return Err(error);
        }
        check(CudaCall::ParseVideoData, result)
    }
}

impl Drop for Parser {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.driver.cuvid.destroy_video_parser)(self.handle) };
        }
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

struct ParserState {
    context: Rc<Context>,
    codec: Codec,
    decoder: Option<Decoder>,
    pool: BufferPool,
    color: SignalledColor,
    pending: Vec<ParserDisplayInfo>,
    ready: Vec<DecodedFrame>,
    error: Option<DecodeError>,
}

impl ParserState {
    fn sequence(&mut self, format: &VideoFormat) -> Result<c_int, DecodeError> {
        let stream = StreamFormat::of(self.codec, format)?;
        let surfaces = c_int::try_from(stream.decode_surfaces).unwrap_or(c_int::MAX);

        if self
            .decoder
            .as_ref()
            .is_some_and(|decoder| decoder.format == stream)
        {
            return Ok(surfaces);
        }

        let earlier = self.download_pending()?;
        self.ready.extend(earlier);
        self.decoder = None;

        let caps = decoder_caps(self.context.driver(), self.codec, stream.pixel_format)?;
        stream.check(self.codec, &caps)?;
        self.decoder = Some(Decoder::create(&self.context, self.codec, stream)?);

        Ok(surfaces)
    }

    fn decode(&mut self, picture: *mut PicParams) -> Result<(), DecodeError> {
        let decoder = self
            .decoder
            .as_ref()
            .ok_or(DecodeError::PictureBeforeSequence { codec: self.codec })?;

        check(CudaCall::DecodePicture, unsafe {
            (self.context.driver().cuvid.decode_picture)(decoder.handle, picture)
        })
    }

    fn download_pending(&mut self) -> Result<Vec<DecodedFrame>, DecodeError> {
        let pending = mem::take(&mut self.pending);
        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(Vec::new());
        };

        pending
            .iter()
            .map(|info| decoder.download(info, &self.pool, self.color))
            .collect()
    }

    fn newest(&mut self) -> Result<Option<DecodedFrame>, DecodeError> {
        let newest = match (self.pending.last().copied(), self.decoder.as_mut()) {
            (Some(info), Some(decoder)) => Some(decoder.download(&info, &self.pool, self.color)?),
            _ => self.ready.pop(),
        };
        self.pending.clear();
        self.ready.clear();

        Ok(newest)
    }

    fn all(&mut self) -> Result<Vec<DecodedFrame>, DecodeError> {
        let later = self.download_pending()?;
        let mut frames = mem::take(&mut self.ready);
        frames.extend(later);

        Ok(frames)
    }
}

unsafe extern "C" fn on_sequence(user: *mut c_void, format: *mut VideoFormat) -> c_int {
    let (Some(state), Some(format)) = (unsafe { user.cast::<ParserState>().as_mut() }, unsafe {
        format.as_ref()
    }) else {
        return CALLBACK_FAILED;
    };

    match state.sequence(format) {
        Ok(surfaces) => surfaces,
        Err(error) => {
            state.error = Some(error);
            CALLBACK_FAILED
        }
    }
}

unsafe extern "C" fn on_decode(user: *mut c_void, picture: *mut PicParams) -> c_int {
    let Some(state) = (unsafe { user.cast::<ParserState>().as_mut() }) else {
        return CALLBACK_FAILED;
    };

    match state.decode(picture) {
        Ok(()) => CALLBACK_SUCCEEDED,
        Err(error) => {
            state.error = Some(error);
            CALLBACK_FAILED
        }
    }
}

unsafe extern "C" fn on_display(user: *mut c_void, info: *mut ParserDisplayInfo) -> c_int {
    let Some(state) = (unsafe { user.cast::<ParserState>().as_mut() }) else {
        return CALLBACK_FAILED;
    };

    if let Some(info) = unsafe { info.as_ref() } {
        state.pending.push(*info);
    }

    CALLBACK_SUCCEEDED
}

struct Decoder {
    context: Rc<Context>,
    handle: CuVideoDecoder,
    format: StreamFormat,
    layout: HostLayout,
    staging: Vec<u8>,
}

impl Decoder {
    fn create(
        context: &Rc<Context>,
        codec: Codec,
        format: StreamFormat,
    ) -> Result<Self, DecodeError> {
        let width = c_ulong::from(format.coded.width);
        let height = c_ulong::from(format.coded.height);
        let mut info = DecodeCreateInfo {
            width,
            height,
            num_decode_surfaces: c_ulong::from(format.decode_surfaces),
            codec: cuda_codec(codec),
            chroma_format: cuda_chroma(format.pixel_format.chroma),
            creation_flags: ffi::CREATE_PREFER_CUVID,
            bit_depth_minus8: c_ulong::from(format.pixel_format.depth.bits() - 8),
            intra_decode_only: 0,
            max_width: width,
            max_height: height,
            reserved1: 0,
            display_area: format.crop,
            output_format: format.surface.code(),
            deinterlace_mode: ffi::DEINTERLACE_MODE_WEAVE,
            target_width: c_ulong::from(format.target.width),
            target_height: c_ulong::from(format.target.height),
            num_output_surfaces: MAPPED_OUTPUT_SURFACES,
            vid_lock: ptr::null_mut(),
            target_rect: ShortRect::default(),
            enable_histogram: 0,
            enable_decode_features: 0,
            reserved2: [0; 3],
        };

        let mut handle: CuVideoDecoder = ptr::null_mut();
        check(CudaCall::CreateDecoder, unsafe {
            (context.driver().cuvid.create_decoder)(&raw mut handle, &raw mut info)
        })?;

        Ok(Self {
            context: Rc::clone(context),
            handle,
            format,
            layout: HostLayout::of(&format),
            staging: Vec::new(),
        })
    }

    fn download(
        &mut self,
        info: &ParserDisplayInfo,
        pool: &BufferPool,
        color: SignalledColor,
    ) -> Result<DecodedFrame, DecodeError> {
        let driver = Rc::clone(self.context.driver());
        let cuvid = &driver.cuvid;
        let mut params = ProcParams::for_display(info);
        let mut device: CuDevicePtr = 0;
        let mut pitch: c_uint = 0;
        check(CudaCall::MapVideoFrame, unsafe {
            (cuvid.map_video_frame)(
                self.handle,
                info.picture_index,
                &raw mut device,
                &raw mut pitch,
                &raw mut params,
            )
        })?;

        let mut buffer = pool.take(self.layout.size);
        let copied = self.copy_planes(device, pitch, &mut buffer);
        unsafe { (cuvid.unmap_video_frame)(self.handle, device) };
        copied?;

        Ok(DecodedFrame {
            resolution: self.format.display,
            format: self.format.pixel_format,
            color,
            timestamp: Timestamp::from_micros(u64::try_from(info.timestamp).unwrap_or(0)),
            buffer: DecodedBuffer::Cpu(HostFrame::new(buffer, self.layout).into_cpu()),
        })
    }

    fn copy_planes(
        &mut self,
        device: CuDevicePtr,
        pitch: c_uint,
        buffer: &mut PooledBuffer,
    ) -> Result<(), DecodeError> {
        let layout = self.layout;
        let bytes = layout.bytes_per_sample;
        let source_pitch = pitch as usize;
        let plane_stride = u64::from(pitch) * u64::from(self.format.target.height);
        let [luma, u, v] = layout.planes;
        let output = buffer.as_mut_slice();

        let (luma_out, chroma_out) = output.split_at_mut(u.offset);
        let (u_out, v_out) = chroma_out.split_at_mut(v.offset - u.offset);
        self.copy(
            device,
            source_pitch,
            luma_out,
            luma.stride,
            luma.row_bytes(bytes),
            luma.height,
        )?;

        if self.format.surface.semi_planar() {
            let mut staging = mem::take(&mut self.staging);
            staging.resize(layout.interleaved_chroma_len(), 0);
            let copied = self.copy(
                device + plane_stride,
                source_pitch,
                &mut staging,
                u.stride * 2,
                u.row_bytes(bytes) * 2,
                u.height,
            );
            deinterleave(&staging, bytes, u_out, v_out);
            self.staging = staging;
            copied?;
        } else {
            self.copy(
                device + plane_stride,
                source_pitch,
                u_out,
                u.stride,
                u.row_bytes(bytes),
                u.height,
            )?;
            self.copy(
                device + 2 * plane_stride,
                source_pitch,
                v_out,
                v.stride,
                v.row_bytes(bytes),
                v.height,
            )?;
        }

        shift_to_low_bits(output, layout.shift);

        Ok(())
    }

    fn copy(
        &self,
        source: CuDevicePtr,
        source_pitch: usize,
        destination: &mut [u8],
        destination_pitch: usize,
        width_in_bytes: usize,
        height: u32,
    ) -> Result<(), DecodeError> {
        let rows = (height as usize).min(destination.len() / destination_pitch.max(1));
        let copy = Memcpy2D::device_to_host(
            source,
            source_pitch,
            destination,
            destination_pitch,
            width_in_bytes.min(destination_pitch),
            rows,
        );

        check(CudaCall::Memcpy2D, unsafe {
            (self.context.driver().cuda.memcpy_2d)(&raw const copy)
        })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _current = self.context.make_current();
        unsafe { (self.context.driver().cuvid.destroy_decoder)(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvdec::driver::{CUDA_LIBRARY, open_library};

    #[test]
    fn opening_without_the_nvidia_driver_is_a_specific_error() {
        let driver_present = open_library(CUDA_LIBRARY).is_ok();

        let result = NvdecDecoder::new(Codec::Hevc, &NvdecDecoderConfig::default());

        if !driver_present {
            assert!(matches!(
                result,
                Err(DecodeError::LibraryUnavailable {
                    library: CUDA_LIBRARY,
                    ..
                })
            ));
        }
    }

    #[test]
    fn capabilities_without_the_nvidia_driver_is_a_specific_error() {
        if open_library(CUDA_LIBRARY).is_ok() {
            return;
        }

        assert!(matches!(
            capabilities(&NvdecDecoderConfig::default()),
            Err(DecodeError::LibraryUnavailable {
                library: CUDA_LIBRARY,
                ..
            })
        ));
    }

    #[test]
    fn a_missing_gpu_index_is_reported_by_index() {
        if open_library(CUDA_LIBRARY).is_err() {
            return;
        }

        let result = NvdecDecoder::new(
            Codec::Hevc,
            &NvdecDecoderConfig {
                gpu_index: u32::MAX,
            },
        );

        assert!(matches!(
            result,
            Err(DecodeError::CudaDeviceMissing {
                index: u32::MAX,
                ..
            } | DecodeError::LibraryUnavailable { .. }
                | DecodeError::Cuda {
                    call: CudaCall::Init,
                    ..
                })
        ));
    }
}
