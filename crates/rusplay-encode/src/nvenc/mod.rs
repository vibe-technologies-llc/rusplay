mod config;
mod driver;
mod ffi;
pub(crate) mod layout;
mod params;
#[cfg(test)]
mod tests;

use std::{
    ffi::c_void,
    fs::File,
    io::{Seek, SeekFrom},
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    rc::Rc,
};

use rusplay_capture::{Frame, FrameBuffer, Plane};
use rusplay_config::{NvencConfig, VideoConfig};
use rusplay_core::{BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution};

use crate::{
    Backend, Capabilities, EncodeError, VideoEncoder,
    input::YuvFrame,
    nvenc::{
        config::{
            Features, Payload, Settings, codec_guid, configure, hdr_payloads, initialize_params,
            preset_guid, profile, rate_control_supported, tuning_info,
        },
        driver::{
            Bitstream, CudaContext, DevicePlane, Driver, Encoder, ExternalBuffer, ExternalMemory,
            InputBuffer, LibraryNames, Registration, SYSTEM_LIBRARIES,
        },
        ffi::{Guid, Plain},
        layout::{
            DmaBufLayout, LINEAR_MODIFIER, Layout, buffer_format, dma_buf_fourcc, dma_buf_layout,
            pack,
        },
        params::NvencParams,
    },
    obu,
};

const BACKEND: Backend = Backend::Nvenc;
const IMPORT_CACHE_CAPACITY: usize = 16;

const fn unsupported(format: PixelFormat) -> EncodeError {
    if matches!(format.chroma, ChromaSubsampling::Yuv420) {
        EncodeError::BitDepthUnsupported {
            backend: BACKEND,
            depth: format.depth,
        }
    } else {
        EncodeError::ChromaUnsupported {
            backend: BACKEND,
            chroma: format.chroma,
        }
    }
}

struct Probe<'a> {
    encoder: &'a Encoder,
    codecs: Vec<Guid>,
}

impl<'a> Probe<'a> {
    fn new(encoder: &'a Encoder) -> Result<Self, EncodeError> {
        Ok(Self {
            encoder,
            codecs: encoder.codecs()?,
        })
    }

    fn has_codec(&self, codec: Codec) -> bool {
        self.codecs.contains(&codec_guid(codec))
    }

    fn supports(&self, codec: Codec, format: PixelFormat) -> Result<bool, EncodeError> {
        let (Some(_), Some(buffer)) = (profile(codec, format), buffer_format(format)) else {
            return Ok(false);
        };
        if !self.has_codec(codec) {
            return Ok(false);
        }

        let guid = codec_guid(codec);
        if !self.encoder.input_formats(guid)?.contains(&buffer) {
            return Ok(false);
        }
        if format.chroma == ChromaSubsampling::Yuv444
            && self.encoder.caps(guid, ffi::CAPS_SUPPORT_YUV444_ENCODE)? == 0
        {
            return Ok(false);
        }
        if format.depth != BitDepth::Eight
            && self.encoder.caps(guid, ffi::CAPS_SUPPORT_10BIT_ENCODE)? == 0
        {
            return Ok(false);
        }

        Ok(true)
    }
}

pub(crate) fn capabilities(nvenc: &NvencConfig) -> Result<Capabilities, EncodeError> {
    capabilities_with(nvenc, SYSTEM_LIBRARIES)
}

fn capabilities_with(
    nvenc: &NvencConfig,
    libraries: LibraryNames,
) -> Result<Capabilities, EncodeError> {
    let driver = Driver::load(libraries)?;
    let context = CudaContext::new(&driver, nvenc.gpu_index)?;
    let _current = context.enter()?;
    let encoder = Encoder::open(Rc::clone(&context))?;
    let probe = Probe::new(&encoder)?;

    Capabilities::probe(BACKEND, |codec, format| probe.supports(codec, format))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    min: Resolution,
    max: Resolution,
}

impl Limits {
    const fn contains(self, resolution: Resolution) -> bool {
        resolution.width >= self.min.width
            && resolution.height >= self.min.height
            && resolution.width <= self.max.width
            && resolution.height <= self.max.height
    }
}

fn caps_u32(encoder: &Encoder, codec: Guid, query: u32) -> Result<u32, EncodeError> {
    Ok(u32::try_from(encoder.caps(codec, query)?).unwrap_or(0))
}

struct Device {
    config: Box<ffi::Config>,
    limits: Limits,
    encoder: Rc<Encoder>,
    context: Rc<CudaContext>,
}

impl Device {
    fn open(
        settings: &Settings,
        nvenc: &NvencConfig,
        libraries: LibraryNames,
    ) -> Result<Self, EncodeError> {
        let driver = Driver::load(libraries)?;
        let context = CudaContext::new(&driver, nvenc.gpu_index)?;
        let _current = context.enter()?;
        let encoder = Encoder::open(Rc::clone(&context))?;

        let probe = Probe::new(&encoder)?;
        if !probe.has_codec(settings.codec) {
            return Err(EncodeError::CodecUnsupported {
                backend: BACKEND,
                codec: settings.codec,
            });
        }
        if !probe.supports(settings.codec, settings.format)? {
            return Err(unsupported(settings.format));
        }

        let guid = codec_guid(settings.codec);
        let modes = encoder.caps(guid, ffi::CAPS_SUPPORTED_RATECONTROL_MODES)?;
        if !rate_control_supported(modes, settings.rate_control) {
            return Err(EncodeError::RateControlUnsupported {
                backend: BACKEND,
                rate_control: settings.rate_control,
            });
        }

        let features = Features {
            intra_refresh: encoder.caps(guid, ffi::CAPS_SUPPORT_INTRA_REFRESH)? != 0,
            temporal_aq: encoder.caps(guid, ffi::CAPS_SUPPORT_TEMPORAL_AQ)? != 0,
        };
        let limits = Limits {
            min: Resolution::new(
                caps_u32(&encoder, guid, ffi::CAPS_WIDTH_MIN)?,
                caps_u32(&encoder, guid, ffi::CAPS_HEIGHT_MIN)?,
            ),
            max: Resolution::new(
                caps_u32(&encoder, guid, ffi::CAPS_WIDTH_MAX)?,
                caps_u32(&encoder, guid, ffi::CAPS_HEIGHT_MAX)?,
            ),
        };

        let mut config = Box::new(encoder.preset_config(
            guid,
            preset_guid(settings.preset),
            tuning_info(settings.tuning),
        )?);
        configure(&mut config, settings, features);
        drop(probe);

        Ok(Self {
            config,
            limits,
            encoder,
            context: Rc::clone(&context),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportSource {
    DmaBuf { device: u64, inode: u64 },
    Opaque { allocation: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportKey {
    source: ImportSource,
    resolution: Resolution,
    planes: Vec<Plane>,
}

#[derive(Debug)]
struct ImportRequest<'a> {
    fd: BorrowedFd<'a>,
    planes: &'a [Plane],
    opaque: Option<OpaqueMemory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpaqueMemory {
    allocation: u64,
    size: u64,
    dedicated: bool,
}

fn import_request(frame: &Frame, format: PixelFormat) -> Result<ImportRequest<'_>, EncodeError> {
    let request = match &frame.buffer {
        FrameBuffer::Shm { .. } => {
            return Err(EncodeError::CpuFrameRequired { backend: BACKEND });
        }
        FrameBuffer::DmaBuf {
            fd,
            modifier,
            planes,
            ..
        } => {
            if *modifier != LINEAR_MODIFIER {
                return Err(EncodeError::DmaBufModifierUnsupported {
                    backend: BACKEND,
                    modifier: *modifier,
                });
            }
            ImportRequest {
                fd: fd.as_fd(),
                planes,
                opaque: None,
            }
        }
        FrameBuffer::OpaqueFd {
            fd,
            allocation,
            size,
            dedicated,
            planes,
        } => ImportRequest {
            fd: fd.as_fd(),
            planes,
            opaque: Some(OpaqueMemory {
                allocation: *allocation,
                size: *size,
                dedicated: *dedicated,
            }),
        },
    };

    let expected = dma_buf_fourcc(format);
    if frame.format != expected {
        return Err(EncodeError::FrameFormatMismatch {
            backend: BACKEND,
            expected,
            actual: frame.format,
        });
    }

    Ok(request)
}

fn check_opaque_size(opaque: OpaqueMemory, layout: DmaBufLayout) -> Result<(), EncodeError> {
    if opaque.size < layout.len {
        return Err(EncodeError::OpaqueFdTooShort {
            needed: layout.len,
            actual: opaque.size,
        });
    }
    Ok(())
}

struct Import {
    key: ImportKey,
    registration: Registration,
    _buffer: ExternalBuffer,
    pitch: u32,
}

#[derive(Debug, Clone, Copy)]
struct Input {
    buffer: *mut c_void,
    pitch: u32,
    format: u32,
}

struct Session {
    imports: Vec<Import>,
    upload: Option<InputBuffer>,
    bitstream: Bitstream,
    config: Box<ffi::Config>,
    resolution: Resolution,
    frame_index: u32,
    needs_keyframe: bool,
}

impl Session {
    fn open(
        device: &Device,
        settings: &Settings,
        resolution: Resolution,
    ) -> Result<Self, EncodeError> {
        if !device.limits.contains(resolution) {
            return Err(EncodeError::ResolutionUnsupported {
                backend: BACKEND,
                resolution,
            });
        }

        let mut config = device.config.clone();
        let mut init = initialize_params(settings, resolution, &mut config);
        device.encoder.initialize(&mut init)?;
        let bitstream = Bitstream::create(&device.encoder)?;

        Ok(Self {
            imports: Vec::new(),
            upload: None,
            bitstream,
            config,
            resolution,
            frame_index: 0,
            needs_keyframe: true,
        })
    }

    fn reconfigure(&mut self, device: &Device, settings: &Settings) -> Result<(), EncodeError> {
        config::apply_rate(&mut self.config.rc_params, settings.rate_values());

        let mut params = ffi::ReconfigureParams::zeroed();
        params.version = ffi::RECONFIGURE_PARAMS_VER;
        params.re_init_encode_params =
            initialize_params(settings, self.resolution, &mut self.config);

        device.encoder.reconfigure(&mut params)
    }

    fn upload(
        &mut self,
        encoder: &Rc<Encoder>,
        view: &YuvFrame<'_>,
        format: PixelFormat,
    ) -> Result<Input, EncodeError> {
        let buffer_format = buffer_format(format).ok_or_else(|| unsupported(format))?;
        let upload = match &mut self.upload {
            Some(upload) => upload,
            empty => empty.insert(InputBuffer::create(
                encoder,
                self.resolution,
                buffer_format,
            )?),
        };

        let rows = Layout::of(format).rows(self.resolution.height);
        let pitch = upload.write(rows, |data, pitch| pack(view, data, pitch))?;

        Ok(Input {
            buffer: upload.pointer(),
            pitch,
            format: upload.format,
        })
    }

    fn import(
        &mut self,
        device: &Device,
        frame: &Frame,
        format: PixelFormat,
    ) -> Result<usize, EncodeError> {
        let request = import_request(frame, format)?;
        let layout = dma_buf_layout(format, frame.resolution, request.planes)?;
        let buffer_format = buffer_format(format).ok_or_else(|| unsupported(format))?;

        let mut file = File::from(
            request
                .fd
                .try_clone_to_owned()
                .map_err(EncodeError::DmaBufInspect)?,
        );
        let source = if let Some(opaque) = request.opaque {
            ImportSource::Opaque {
                allocation: opaque.allocation,
            }
        } else {
            let metadata = file.metadata().map_err(EncodeError::DmaBufInspect)?;
            ImportSource::DmaBuf {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        };
        let key = ImportKey {
            source,
            resolution: frame.resolution,
            planes: request.planes.to_vec(),
        };

        if let Some(position) = self.imports.iter().position(|cached| cached.key == key) {
            let entry = self.imports.remove(position);
            self.imports.push(entry);
            return Ok(self.imports.len() - 1);
        }

        let memory = if let Some(opaque) = request.opaque {
            check_opaque_size(opaque, layout)?;
            ExternalMemory {
                size: opaque.size,
                dedicated: opaque.dedicated,
            }
        } else {
            let size = file
                .seek(SeekFrom::End(0))
                .map_err(EncodeError::DmaBufInspect)?;
            if size < layout.len {
                return Err(EncodeError::DmaBufTooShort {
                    needed: layout.len,
                    actual: size,
                });
            }
            ExternalMemory {
                size,
                dedicated: false,
            }
        };

        let buffer = ExternalBuffer::import(&device.context, OwnedFd::from(file), memory)?;
        let registration = Registration::register(
            &device.encoder,
            DevicePlane {
                pointer: buffer.pointer() + layout.base,
                resolution: frame.resolution,
                pitch: layout.pitch,
                format: buffer_format,
            },
        )?;

        if self.imports.len() >= IMPORT_CACHE_CAPACITY {
            self.imports.remove(0);
        }
        self.imports.push(Import {
            key,
            registration,
            _buffer: buffer,
            pitch: layout.pitch,
        });

        Ok(self.imports.len() - 1)
    }
}

#[derive(Debug, Clone, Copy)]
struct Picture {
    keyframe: bool,
    index: u32,
    timestamp: u64,
    resolution: Resolution,
}

fn sei_array(payloads: &mut [Payload]) -> Vec<ffi::SeiPayload> {
    payloads
        .iter_mut()
        .map(|payload| ffi::SeiPayload {
            payload_size: u32::try_from(payload.bytes.len()).unwrap_or(u32::MAX),
            payload_type: payload.kind,
            payload: payload.bytes.as_mut_ptr(),
        })
        .collect()
}

fn picture_params(
    codec: Codec,
    picture: Picture,
    input: Input,
    output: *mut c_void,
    sei: &mut [ffi::SeiPayload],
) -> ffi::PicParams {
    let mut params = ffi::PicParams::zeroed();
    params.version = ffi::PIC_PARAMS_VER;
    params.input_width = picture.resolution.width;
    params.input_height = picture.resolution.height;
    params.input_pitch = input.pitch;
    params.frame_idx = picture.index;
    params.input_time_stamp = picture.timestamp;
    params.input_buffer = input.buffer;
    params.output_bitstream = output;
    params.buffer_fmt = input.format;
    params.picture_struct = ffi::PIC_STRUCT_FRAME;
    if picture.keyframe {
        params.encode_pic_flags = ffi::PIC_FLAG_FORCE_IDR | ffi::PIC_FLAG_OUTPUT_SPSPPS;
    }

    if !sei.is_empty() {
        let count = u32::try_from(sei.len()).unwrap_or(u32::MAX);
        let array = sei.as_mut_ptr();
        match codec {
            Codec::H264 => {
                let mut h264 = ffi::PicParamsH264::zeroed();
                h264.sei_payload_array_cnt = count;
                h264.sei_payload_array = array;
                params.codec.h264 = h264;
            }
            Codec::Hevc => {
                let mut hevc = ffi::PicParamsHevc::zeroed();
                hevc.sei_payload_array_cnt = count;
                hevc.sei_payload_array = array;
                params.codec.hevc = hevc;
            }
            Codec::Av1 => {
                let mut av1 = ffi::PicParamsAv1::zeroed();
                av1.obu_payload_array_cnt = count;
                av1.obu_payload_array = array;
                params.codec.av1 = av1;
            }
        }
    }

    params
}

pub struct NvencEncoder {
    session: Option<Session>,
    device: Device,
    settings: Settings,
    keyframe_requested: bool,
    rate_changed: bool,
}

unsafe impl Send for NvencEncoder {}

impl NvencEncoder {
    pub fn new(video: &VideoConfig, nvenc: &NvencConfig) -> Result<Self, EncodeError> {
        Self::new_with(video, nvenc, SYSTEM_LIBRARIES)
    }

    fn new_with(
        video: &VideoConfig,
        nvenc: &NvencConfig,
        libraries: LibraryNames,
    ) -> Result<Self, EncodeError> {
        let params = NvencParams::parse(&nvenc.params)?;
        if video.color.primaries.h273_code().is_none() {
            return Err(EncodeError::ColorPrimariesUnsignalable {
                backend: BACKEND,
                primaries: video.color.primaries,
            });
        }

        let settings = Settings {
            codec: video.codec,
            format: video.pixel_format,
            color: video.color,
            frame_rate: video.frame_rate,
            resolution: video.resolution,
            bitrate_kbps: video.bitrate_kbps,
            rate_control: nvenc.rate_control,
            preset: nvenc.preset,
            tuning: nvenc.tuning,
            params,
        };
        let device = Device::open(&settings, nvenc, libraries)?;

        let session = {
            let _current = device.context.enter()?;
            settings
                .resolution
                .map(|resolution| Session::open(&device, &settings, resolution))
                .transpose()?
        };

        Ok(Self {
            session,
            device,
            settings,
            keyframe_requested: false,
            rate_changed: false,
        })
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        let _current = self.device.context.enter();
        self.session = None;
    }
}

fn session_for<'a>(
    slot: &'a mut Option<Session>,
    device: &Device,
    settings: &Settings,
    resolution: Resolution,
) -> Result<&'a mut Session, EncodeError> {
    let session = match slot {
        Some(session) => session,
        empty => empty.insert(Session::open(device, settings, resolution)?),
    };

    if session.resolution == resolution {
        Ok(session)
    } else {
        Err(EncodeError::FrameResolutionMismatch {
            backend: BACKEND,
            expected: session.resolution,
            actual: resolution,
        })
    }
}

impl VideoEncoder for NvencEncoder {
    fn encode(&mut self, frame: &Frame) -> Result<Option<EncodedFrame>, EncodeError> {
        let Self {
            session: slot,
            device,
            settings,
            keyframe_requested,
            rate_changed,
        } = self;

        if let Some(expected) = settings.resolution
            && frame.resolution != expected
        {
            return Err(EncodeError::FrameResolutionMismatch {
                backend: BACKEND,
                expected,
                actual: frame.resolution,
            });
        }

        let _current = device.context.enter()?;
        let session = session_for(slot, device, settings, frame.resolution)?;
        if std::mem::take(rate_changed) {
            session.reconfigure(device, settings)?;
        }

        let requested = std::mem::take(keyframe_requested);
        let keyframe = session.needs_keyframe || requested;
        let mut payloads = if keyframe {
            hdr_payloads(settings.codec, &settings.color.hdr)
        } else {
            Vec::new()
        };
        let mut sei = sei_array(&mut payloads);
        let picture = Picture {
            keyframe,
            index: session.frame_index,
            timestamp: frame.timestamp.as_micros(),
            resolution: frame.resolution,
        };

        let coded = match &frame.buffer {
            FrameBuffer::Shm { .. } => {
                let view = YuvFrame::from_frame(frame, BACKEND, settings.format)?;
                let input = session.upload(&device.encoder, &view, settings.format)?;
                let mut params = picture_params(
                    settings.codec,
                    picture,
                    input,
                    session.bitstream.pointer(),
                    &mut sei,
                );
                device.encoder.encode(&mut params)?;
                session.bitstream.read()?
            }
            FrameBuffer::DmaBuf { .. } | FrameBuffer::OpaqueFd { .. } => {
                let index = session.import(device, frame, settings.format)?;
                let import = &session.imports[index];
                let mapped = import.registration.map()?;
                let input = Input {
                    buffer: mapped.input,
                    pitch: import.pitch,
                    format: mapped.format,
                };
                let mut params = picture_params(
                    settings.codec,
                    picture,
                    input,
                    session.bitstream.pointer(),
                    &mut sei,
                );
                device.encoder.encode(&mut params)?;
                let coded = session.bitstream.read()?;
                drop(mapped);
                coded
            }
        };

        session.needs_keyframe = false;
        session.frame_index = session.frame_index.wrapping_add(1);

        if coded.data.is_empty() {
            return Ok(None);
        }
        let data = match settings.codec {
            Codec::Av1 => obu::delimited(coded.data),
            Codec::H264 | Codec::Hevc => coded.data,
        };

        Ok(Some(EncodedFrame {
            codec: settings.codec,
            timestamp: frame.timestamp,
            keyframe: keyframe || coded.picture_type == ffi::PIC_TYPE_IDR,
            data,
        }))
    }

    fn request_keyframe(&mut self) {
        self.keyframe_requested = true;
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), EncodeError> {
        if kbps == 0 {
            return Err(EncodeError::BitrateRejected {
                backend: BACKEND,
                kbps,
            });
        }

        self.settings.bitrate_kbps = kbps;
        self.rate_changed = true;

        Ok(())
    }
}
