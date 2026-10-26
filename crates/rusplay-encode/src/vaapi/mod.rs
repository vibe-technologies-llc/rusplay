mod av1;
mod bits;
mod ffi;
mod format;
mod h264;
mod hdr;
mod hevc;
mod level;
mod params;
#[cfg(test)]
mod tests;
mod va;

use std::{
    fs::File,
    io::{Seek, SeekFrom},
    os::{fd::AsFd, unix::fs::MetadataExt},
    rc::Rc,
};

use rusplay_capture::{Frame, FrameBuffer, Plane};
use rusplay_config::{RateControl, VaapiConfig, VideoConfig};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorSpace, EncodedFrame, FrameRate, PixelFormat,
    Resolution,
};

use crate::{
    Backend, Capabilities, EncodeError, VideoEncoder,
    input::YuvFrame,
    obu,
    rate::vbv_buffer_kbit,
    vaapi::{
        av1::{Av1Profile, Av1Stream},
        format::{
            coded_bytes_per_pixel_x2, dma_buf_fourcc, dma_buf_planes, rt_format, surface_format,
        },
        h264::{H264Profile, H264Stream},
        hevc::{HevcProfile, HevcStream},
        params::VaapiParams,
        va::{
            Buffer, CodedBuffer, Config, Context, Display, DmaBufImport, DmaBufPlaneLayout,
            Entrypoint, Image, PackedHeader, SizeLimits, Surface, SurfaceId,
        },
    },
};

const BACKEND: Backend = Backend::Vaapi;
const YUV420_8: PixelFormat = PixelFormat {
    chroma: ChromaSubsampling::Yuv420,
    depth: BitDepth::Eight,
};
const INFINITE_INTRA_PERIOD: u32 = 0xffff;
const RECONSTRUCTED_ALIGNMENT: u32 = 16;
const CODED_BUFFER_HEADROOM: u64 = 64 * 1024;
const IMPORT_CACHE_CAPACITY: usize = 16;
const WANTED_PACKED_HEADERS: u32 = ffi::VA_ENC_PACKED_HEADER_SEQUENCE
    | ffi::VA_ENC_PACKED_HEADER_PICTURE
    | ffi::VA_ENC_PACKED_HEADER_SLICE
    | ffi::VA_ENC_PACKED_HEADER_RAW_DATA;

#[derive(Debug, Clone, Copy)]
struct StreamInfo {
    resolution: Resolution,
    format: PixelFormat,
    color: ColorSpace,
    frame_rate: FrameRate,
    peak_kbps: u32,
    qp: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
struct Picture {
    keyframe: bool,
    index: u64,
    recon: SurfaceId,
    coded: ffi::VABufferID,
    bits_per_second: u32,
    packed_headers: u32,
}

struct CodecBuffers {
    sequence: Option<Buffer>,
    picture: Buffer,
    headers: Vec<Buffer>,
    slices: Vec<Buffer>,
}

fn sequence_buffer<T: Copy>(
    context: &Context,
    picture: &Picture,
    parameters: impl FnOnce() -> T,
) -> Result<Option<Buffer>, EncodeError> {
    picture
        .keyframe
        .then(|| {
            context.parameter(
                ffi::VABufferType_VAEncSequenceParameterBufferType,
                &parameters(),
            )
        })
        .transpose()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackedKind {
    Sequence,
    Picture,
    Raw,
    Slice,
}

impl PackedKind {
    const fn flag(self) -> u32 {
        match self {
            Self::Sequence => ffi::VA_ENC_PACKED_HEADER_SEQUENCE,
            Self::Picture => ffi::VA_ENC_PACKED_HEADER_PICTURE,
            Self::Raw => ffi::VA_ENC_PACKED_HEADER_RAW_DATA,
            Self::Slice => ffi::VA_ENC_PACKED_HEADER_SLICE,
        }
    }

    const fn va(self) -> ffi::VAEncPackedHeaderType {
        match self {
            Self::Sequence => ffi::VAEncPackedHeaderType_VAEncPackedHeaderSequence,
            Self::Picture => ffi::VAEncPackedHeaderType_VAEncPackedHeaderPicture,
            Self::Raw => ffi::VAEncPackedHeaderType_VAEncPackedHeaderRawData,
            Self::Slice => ffi::VAEncPackedHeaderType_VAEncPackedHeaderSlice,
        }
    }
}

fn packed_buffers<const N: usize>(
    context: &Context,
    picture: &Picture,
    headers: [(PackedKind, Option<PackedHeader>); N],
) -> Result<Vec<Buffer>, EncodeError> {
    let mut buffers = Vec::new();
    for (kind, header) in headers {
        if let Some(header) = header.filter(|_| picture.packed_headers & kind.flag() != 0) {
            buffers.extend(context.packed_header(kind.va(), &header)?);
        }
    }

    Ok(buffers)
}

fn codec_buffers<P: Copy, S: Copy>(
    context: &Context,
    sequence: Option<Buffer>,
    picture: &P,
    headers: Vec<Buffer>,
    slice: &S,
) -> Result<CodecBuffers, EncodeError> {
    Ok(CodecBuffers {
        sequence,
        picture: context.parameter(ffi::VABufferType_VAEncPictureParameterBufferType, picture)?,
        headers,
        slices: vec![context.parameter(ffi::VABufferType_VAEncSliceParameterBufferType, slice)?],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodecProfile {
    H264(H264Profile),
    Hevc(HevcProfile),
    Av1(Av1Profile),
}

impl CodecProfile {
    const fn va(self) -> va::Profile {
        match self {
            Self::H264(profile) => profile.va(),
            Self::Hevc(profile) => profile.va(),
            Self::Av1(profile) => profile.va(),
        }
    }

    const fn refresh_block_size(self) -> u32 {
        match self {
            Self::H264(_) => H264Stream::refresh_block_size(),
            Self::Hevc(_) => HevcStream::refresh_block_size(),
            Self::Av1(_) => Av1Stream::refresh_block_size(),
        }
    }
}

const fn unsupported(codec: Codec, format: PixelFormat) -> EncodeError {
    if !matches!(format.chroma, ChromaSubsampling::Yuv420) {
        EncodeError::ChromaUnsupported {
            backend: BACKEND,
            chroma: format.chroma,
        }
    } else if format.depth.bits() != 8 {
        EncodeError::BitDepthUnsupported {
            backend: BACKEND,
            depth: format.depth,
        }
    } else {
        EncodeError::CodecUnsupported {
            backend: BACKEND,
            codec,
        }
    }
}

fn candidates(codec: Codec, format: PixelFormat) -> Vec<CodecProfile> {
    match codec {
        Codec::H264 if format == YUV420_8 => H264Profile::ORDER
            .into_iter()
            .map(CodecProfile::H264)
            .collect(),
        Codec::H264 => Vec::new(),
        Codec::Hevc => vec![CodecProfile::Hevc(HevcProfile::for_format(format))],
        Codec::Av1 => Av1Profile::for_format(format)
            .map(CodecProfile::Av1)
            .into_iter()
            .collect(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    profile: CodecProfile,
    entrypoint: Entrypoint,
}

fn preferred_entrypoints(low_power: bool) -> [Entrypoint; 2] {
    if low_power {
        [
            ffi::VAEntrypoint_VAEntrypointEncSliceLP,
            ffi::VAEntrypoint_VAEntrypointEncSlice,
        ]
    } else {
        [
            ffi::VAEntrypoint_VAEntrypointEncSlice,
            ffi::VAEntrypoint_VAEntrypointEncSliceLP,
        ]
    }
}

fn select(
    display: &Display,
    codec: Codec,
    format: PixelFormat,
    low_power: bool,
) -> Result<Option<Selection>, EncodeError> {
    let profiles = display.profiles()?;

    for profile in candidates(codec, format) {
        if !profiles.contains(&profile.va()) {
            continue;
        }

        let entrypoints = display.entrypoints(profile.va())?;
        for entrypoint in preferred_entrypoints(low_power) {
            if !entrypoints.contains(&entrypoint) {
                continue;
            }

            let [rt_formats] = display.attributes(
                profile.va(),
                entrypoint,
                [ffi::VAConfigAttribType_VAConfigAttribRTFormat],
            )?;
            if rt_formats.is_some_and(|formats| formats & rt_format(format) != 0) {
                return Ok(Some(Selection {
                    profile,
                    entrypoint,
                }));
            }
        }
    }

    Ok(None)
}

pub(crate) fn capabilities(vaapi: &VaapiConfig) -> Result<Capabilities, EncodeError> {
    let display = Display::open(&vaapi.device)?;

    Capabilities::probe(BACKEND, |codec, format| {
        Ok(select(&display, codec, format, vaapi.low_power)?.is_some())
    })
}

const fn rate_control_mode(rate_control: RateControl) -> u32 {
    match rate_control {
        RateControl::Cbr => ffi::VA_RC_CBR,
        RateControl::Vbr { .. } => ffi::VA_RC_VBR,
        RateControl::ConstantQuality { .. } => ffi::VA_RC_CQP,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshDirection {
    Rows,
    Columns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IntraRefresh {
    direction: RefreshDirection,
    period: u32,
}

impl IntraRefresh {
    fn flags(self) -> u32 {
        match self.direction {
            RefreshDirection::Rows => ffi::VA_ENC_INTRA_REFRESH_ROLLING_ROW,
            RefreshDirection::Columns => ffi::VA_ENC_INTRA_REFRESH_ROLLING_COLUMN,
        }
    }

    fn units(self, resolution: Resolution, block: u32) -> u32 {
        match self.direction {
            RefreshDirection::Rows => resolution.height.div_ceil(block),
            RefreshDirection::Columns => resolution.width.div_ceil(block),
        }
    }
}

struct Device {
    selection: Selection,
    config: Config,
    display: Rc<Display>,
    limits: SizeLimits,
    packed_headers: u32,
    intra_refresh: Option<IntraRefresh>,
}

impl Device {
    fn open(
        video: &VideoConfig,
        vaapi: &VaapiConfig,
        params: &VaapiParams,
    ) -> Result<Self, EncodeError> {
        let display = Display::open(&vaapi.device)?;
        let format = video.pixel_format;

        let selection = select(&display, video.codec, format, vaapi.low_power)?
            .ok_or_else(|| unsupported(video.codec, format))?;
        let profile = selection.profile.va();

        let [
            rate_controls,
            packed,
            refresh,
            quality_levels,
            max_width,
            max_height,
        ] = display.attributes(
            profile,
            selection.entrypoint,
            [
                ffi::VAConfigAttribType_VAConfigAttribRateControl,
                ffi::VAConfigAttribType_VAConfigAttribEncPackedHeaders,
                ffi::VAConfigAttribType_VAConfigAttribEncIntraRefresh,
                ffi::VAConfigAttribType_VAConfigAttribEncQualityRange,
                ffi::VAConfigAttribType_VAConfigAttribMaxPictureWidth,
                ffi::VAConfigAttribType_VAConfigAttribMaxPictureHeight,
            ],
        )?;

        let mode = rate_control_mode(vaapi.rate_control);
        if rate_controls.unwrap_or(0) & mode == 0 {
            return Err(EncodeError::RateControlUnsupported {
                backend: BACKEND,
                rate_control: vaapi.rate_control,
            });
        }

        params.check_quality_level(quality_levels)?;

        let packed_headers = packed.unwrap_or(0) & WANTED_PACKED_HEADERS;
        let period = params
            .intra_refresh_frames
            .unwrap_or_else(|| default_refresh_period(video.frame_rate));
        let refresh = refresh.unwrap_or(0);
        let direction = if refresh & ffi::VA_ENC_INTRA_REFRESH_ROLLING_ROW != 0 {
            Some(RefreshDirection::Rows)
        } else if refresh & ffi::VA_ENC_INTRA_REFRESH_ROLLING_COLUMN != 0 {
            Some(RefreshDirection::Columns)
        } else {
            None
        };
        let intra_refresh = direction
            .filter(|_| period > 0)
            .map(|direction| IntraRefresh { direction, period });

        let config = Config::new(
            &display,
            profile,
            selection.entrypoint,
            &[
                (
                    ffi::VAConfigAttribType_VAConfigAttribRTFormat,
                    rt_format(format),
                ),
                (ffi::VAConfigAttribType_VAConfigAttribRateControl, mode),
                (
                    ffi::VAConfigAttribType_VAConfigAttribEncPackedHeaders,
                    packed_headers,
                ),
            ],
        )?;

        let mut limits = config.size_limits()?;
        if let Some(width) = max_width {
            limits.max.width = limits.max.width.min(width);
        }
        if let Some(height) = max_height {
            limits.max.height = limits.max.height.min(height);
        }

        Ok(Self {
            selection,
            config,
            display,
            limits,
            packed_headers,
            intra_refresh,
        })
    }
}

fn default_refresh_period(frame_rate: FrameRate) -> u32 {
    frame_rate
        .numerator
        .div_ceil(frame_rate.denominator.max(1))
        .max(1)
}

#[derive(Debug, Clone)]
struct Settings {
    codec: Codec,
    format: PixelFormat,
    color: ColorSpace,
    frame_rate: FrameRate,
    resolution: Option<Resolution>,
    bitrate_kbps: u32,
    rate_control: RateControl,
    params: VaapiParams,
}

impl Settings {
    fn bits_per_second(&self) -> u32 {
        self.peak_kbps().saturating_mul(1000)
    }

    fn peak_kbps(&self) -> u32 {
        match self.rate_control {
            RateControl::Vbr { max_bitrate_kbps } => max_bitrate_kbps.max(self.bitrate_kbps),
            RateControl::Cbr | RateControl::ConstantQuality { .. } => self.bitrate_kbps,
        }
    }

    fn qp(&self) -> Option<u8> {
        match self.rate_control {
            RateControl::ConstantQuality { qp } => Some(qp),
            RateControl::Cbr | RateControl::Vbr { .. } => None,
        }
    }

    fn info(&self, resolution: Resolution) -> StreamInfo {
        StreamInfo {
            resolution,
            format: self.format,
            color: self.color,
            frame_rate: self.frame_rate,
            peak_kbps: self.peak_kbps(),
            qp: self.qp(),
        }
    }
}

enum Stream {
    H264(Box<H264Stream>),
    Hevc(Box<HevcStream>),
    Av1(Box<Av1Stream>),
}

impl Stream {
    fn new(profile: CodecProfile, info: StreamInfo) -> Self {
        match profile {
            CodecProfile::H264(profile) => Self::H264(Box::new(H264Stream::new(profile, info))),
            CodecProfile::Hevc(profile) => Self::Hevc(Box::new(HevcStream::new(profile, info))),
            CodecProfile::Av1(profile) => Self::Av1(Box::new(Av1Stream::new(profile, info))),
        }
    }

    fn buffers(
        &mut self,
        context: &Context,
        picture: &Picture,
    ) -> Result<CodecBuffers, EncodeError> {
        match self {
            Self::H264(stream) => stream.buffers(context, picture),
            Self::Hevc(stream) => stream.buffers(context, picture),
            Self::Av1(stream) => stream.buffers(context, picture),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportKey {
    device: u64,
    inode: u64,
    modifier: u64,
    resolution: Resolution,
    planes: Vec<Plane>,
}

struct Upload {
    image: Image,
    surface: Surface,
}

struct Session {
    stream: Stream,
    coded: CodedBuffer,
    context: Context,
    upload: Option<Upload>,
    imports: Vec<(ImportKey, Surface)>,
    recon: [Surface; 2],
    resolution: Resolution,
    current: usize,
    index: u64,
    needs_keyframe: bool,
    refresh_cursor: u32,
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

        let aligned = Resolution::new(
            resolution.width.next_multiple_of(RECONSTRUCTED_ALIGNMENT),
            resolution.height.next_multiple_of(RECONSTRUCTED_ALIGNMENT),
        );
        let format = surface_format(settings.format);
        let recon = [
            Surface::create(&device.display, format, aligned)?,
            Surface::create(&device.display, format, aligned)?,
        ];
        let context = Context::new(
            &device.config,
            aligned.width,
            aligned.height,
            &[recon[0].id(), recon[1].id()],
        )?;
        let capacity = aligned.pixel_count() * coded_bytes_per_pixel_x2(settings.format) / 2
            + CODED_BUFFER_HEADROOM;
        let coded = context.coded(usize::try_from(capacity).unwrap_or(usize::MAX))?;

        Ok(Self {
            stream: Stream::new(device.selection.profile, settings.info(resolution)),
            coded,
            context,
            upload: None,
            imports: Vec::new(),
            recon,
            resolution,
            current: 0,
            index: 0,
            needs_keyframe: true,
            refresh_cursor: 0,
        })
    }

    fn advance_refresh(&mut self, refresh: IntraRefresh, block: u32) -> (u32, u32, u32) {
        let units = refresh.units(self.resolution, block).max(1);
        let size = units.div_ceil(refresh.period).max(1);
        let location = self.refresh_cursor;
        self.refresh_cursor = if location + size >= units {
            0
        } else {
            location + size
        };

        (refresh.flags(), location, size)
    }

    fn upload(
        &mut self,
        display: &Rc<Display>,
        view: &YuvFrame<'_>,
    ) -> Result<SurfaceId, EncodeError> {
        let upload = match &mut self.upload {
            Some(upload) => upload,
            empty => {
                let format = surface_format(view.format);
                let image = Image::new(display, format.va_fourcc, view.resolution)?;
                let surface = Surface::create(display, format, view.resolution)?;
                empty.insert(Upload { image, surface })
            }
        };

        upload
            .image
            .write(|data, layout| format::pack(view, data, layout))?;
        upload.image.put(&upload.surface)?;

        Ok(upload.surface.id())
    }

    fn import(
        &mut self,
        display: &Rc<Display>,
        frame: &Frame,
        format: PixelFormat,
    ) -> Result<SurfaceId, EncodeError> {
        let FrameBuffer::DmaBuf {
            fd,
            modifier,
            planes,
            ..
        } = &frame.buffer
        else {
            return Err(EncodeError::CpuFrameRequired { backend: BACKEND });
        };

        let expected_fourcc = dma_buf_fourcc(format);
        if frame.format != expected_fourcc {
            return Err(EncodeError::FrameFormatMismatch {
                backend: BACKEND,
                expected: expected_fourcc,
                actual: frame.format,
            });
        }
        let expected_planes = dma_buf_planes(format);
        if planes.len() != expected_planes {
            return Err(EncodeError::DmaBufPlaneCount {
                fourcc: expected_fourcc,
                expected: expected_planes,
                actual: planes.len(),
            });
        }

        let mut file = File::from(fd.try_clone().map_err(EncodeError::DmaBufInspect)?);
        let metadata = file.metadata().map_err(EncodeError::DmaBufInspect)?;
        let key = ImportKey {
            device: metadata.dev(),
            inode: metadata.ino(),
            modifier: *modifier,
            resolution: frame.resolution,
            planes: planes.clone(),
        };

        if let Some(position) = self.imports.iter().position(|(cached, _)| *cached == key) {
            let entry = self.imports.remove(position);
            let id = entry.1.id();
            self.imports.push(entry);
            return Ok(id);
        }

        let size = file
            .seek(SeekFrom::End(0))
            .map_err(EncodeError::DmaBufInspect)?;
        let layouts: Vec<DmaBufPlaneLayout> = planes
            .iter()
            .map(|plane| DmaBufPlaneLayout {
                offset: plane.offset,
                pitch: plane.stride,
            })
            .collect();
        let surface = Surface::import(
            display,
            surface_format(format),
            &DmaBufImport {
                fd: file.as_fd(),
                size: u32::try_from(size).unwrap_or(u32::MAX),
                modifier: *modifier,
                resolution: frame.resolution,
                planes: &layouts,
            },
        )?;

        if self.imports.len() >= IMPORT_CACHE_CAPACITY {
            self.imports.remove(0);
        }
        let id = surface.id();
        self.imports.push((key, surface));

        Ok(id)
    }
}

pub struct VaapiEncoder {
    session: Option<Session>,
    device: Device,
    settings: Settings,
    keyframe_requested: bool,
    rate_changed: bool,
}

unsafe impl Send for VaapiEncoder {}

impl VaapiEncoder {
    pub fn new(video: &VideoConfig, vaapi: &VaapiConfig) -> Result<Self, EncodeError> {
        let params = VaapiParams::parse(&vaapi.params)?;
        if video.color.primaries.h273_code().is_none() {
            return Err(EncodeError::ColorPrimariesUnsignalable {
                backend: BACKEND,
                primaries: video.color.primaries,
            });
        }
        let device = Device::open(video, vaapi, &params)?;

        let settings = Settings {
            codec: video.codec,
            format: video.pixel_format,
            color: video.color,
            frame_rate: video.frame_rate,
            resolution: video.resolution,
            bitrate_kbps: video.bitrate_kbps,
            rate_control: vaapi.rate_control,
            params,
        };

        let session = settings
            .resolution
            .map(|resolution| Session::open(&device, &settings, resolution))
            .transpose()?;

        Ok(Self {
            session,
            device,
            settings,
            keyframe_requested: false,
            rate_changed: false,
        })
    }
}

fn rate_buffers(settings: &Settings, context: &Context) -> Result<Vec<Buffer>, EncodeError> {
    let peak = settings.peak_kbps();
    let target_percentage = match settings.rate_control {
        RateControl::ConstantQuality { .. } => return Ok(Vec::new()),
        RateControl::Cbr => 100,
        RateControl::Vbr { .. } => {
            (u64::from(settings.bitrate_kbps) * 100 / u64::from(peak.max(1))).clamp(1, 100)
        }
    };

    let buffer_bits = vbv_buffer_kbit(peak, settings.frame_rate).saturating_mul(1000);
    let window_ms = u64::from(buffer_bits) * 1000 / u64::from(settings.bits_per_second().max(1));

    let mut rate_control = ffi::VAEncMiscParameterRateControl {
        bits_per_second: settings.bits_per_second(),
        target_percentage: u32::try_from(target_percentage).unwrap_or(100),
        window_size: u32::try_from(window_ms.max(1)).unwrap_or(u32::MAX),
        ..Default::default()
    };
    unsafe { rate_control.rc_flags.bits.set_disable_bit_stuffing(1) };

    let hrd = ffi::VAEncMiscParameterHRD {
        initial_buffer_fullness: buffer_bits,
        buffer_size: buffer_bits,
        ..Default::default()
    };

    Ok(vec![
        context.misc(
            ffi::VAEncMiscParameterType_VAEncMiscParameterTypeRateControl,
            &rate_control,
        )?,
        context.misc(ffi::VAEncMiscParameterType_VAEncMiscParameterTypeHRD, &hrd)?,
    ])
}

fn sequence_misc_buffers(
    settings: &Settings,
    context: &Context,
) -> Result<Vec<Buffer>, EncodeError> {
    let frame_rate = ffi::VAEncMiscParameterFrameRate {
        framerate: packed_frame_rate(settings.frame_rate),
        ..Default::default()
    };
    let mut buffers = vec![context.misc(
        ffi::VAEncMiscParameterType_VAEncMiscParameterTypeFrameRate,
        &frame_rate,
    )?];

    if let Some(quality_level) = settings.params.quality_level {
        let quality = ffi::VAEncMiscParameterBufferQualityLevel {
            quality_level,
            ..Default::default()
        };
        buffers.push(context.misc(
            ffi::VAEncMiscParameterType_VAEncMiscParameterTypeQualityLevel,
            &quality,
        )?);
    }

    Ok(buffers)
}

fn refresh_buffer(
    context: &Context,
    flags: u32,
    location: u32,
    size: u32,
) -> Result<Buffer, EncodeError> {
    let mut parameters = ffi::VAEncMiscParameterRIR {
        intra_insertion_location: u16::try_from(location).unwrap_or(0),
        intra_insert_size: u16::try_from(size).unwrap_or(u16::MAX),
        ..Default::default()
    };
    parameters.rir_flags.value = flags;

    context.misc(
        ffi::VAEncMiscParameterType_VAEncMiscParameterTypeRIR,
        &parameters,
    )
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

fn packed_frame_rate(frame_rate: FrameRate) -> u32 {
    let mut numerator = frame_rate.numerator.max(1);
    let mut denominator = frame_rate.denominator.max(1);
    while numerator > 0xffff || denominator > 0xffff {
        numerator = (numerator / 2).max(1);
        denominator = (denominator / 2).max(1);
    }

    if denominator == 1 {
        numerator
    } else {
        numerator | (denominator << 16)
    }
}

impl VideoEncoder for VaapiEncoder {
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

        let session = session_for(slot, device, settings, frame.resolution)?;
        let input = match &frame.buffer {
            FrameBuffer::Shm { .. } => {
                let view = YuvFrame::from_frame(frame, BACKEND, settings.format)?;
                session.upload(&device.display, &view)?
            }
            FrameBuffer::DmaBuf { .. } => {
                session.import(&device.display, frame, settings.format)?
            }
        };

        let requested = std::mem::take(keyframe_requested);
        let keyframe = session.needs_keyframe || requested;
        if keyframe {
            session.index = 0;
            session.refresh_cursor = 0;
        }
        let picture = Picture {
            keyframe,
            index: session.index,
            recon: session.recon[session.current].id(),
            coded: session.coded.id(),
            bits_per_second: settings.bits_per_second(),
            packed_headers: device.packed_headers,
        };
        let refresh = match device.intra_refresh {
            Some(refresh) if !keyframe => {
                let block = device.selection.profile.refresh_block_size();
                Some(session.advance_refresh(refresh, block))
            }
            _ => None,
        };

        let codec_buffers = session.stream.buffers(&session.context, &picture)?;
        let context = &session.context;
        let mut buffers = Vec::new();
        buffers.extend(codec_buffers.sequence);
        if std::mem::take(rate_changed) || keyframe {
            buffers.extend(rate_buffers(settings, context)?);
        }
        if keyframe {
            buffers.extend(sequence_misc_buffers(settings, context)?);
        }
        if let Some((flags, location, size)) = refresh {
            buffers.push(refresh_buffer(context, flags, location, size)?);
        }
        buffers.push(codec_buffers.picture);
        buffers.extend(codec_buffers.headers);
        buffers.extend(codec_buffers.slices);

        context.render(input, &buffers)?;
        device.display.sync(input)?;
        let mut data = session.coded.read()?;
        drop(buffers);

        if settings.codec == Codec::Av1 {
            data = obu::delimited(data);
        }

        session.needs_keyframe = false;
        session.current ^= 1;
        session.index += 1;

        Ok(Some(EncodedFrame {
            codec: settings.codec,
            timestamp: frame.timestamp,
            keyframe,
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
