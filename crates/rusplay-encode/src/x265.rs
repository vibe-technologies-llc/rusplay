use std::{
    ffi::{CStr, c_char, c_int, c_void},
    mem,
    ptr::{self, NonNull},
    slice,
};

use rusplay_capture::Frame;
use rusplay_config::{VideoConfig, X265Config, X265Tune};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorRange, ColorSpace, EncodedFrame, FrameRate,
    PixelFormat, Resolution, Timestamp,
};

use crate::{
    Backend, EncodeError, VideoEncoder,
    input::YuvFrame,
    rate::vbv_buffer_kbit,
    software::CParam,
    x26x::{colour_matrix_name, colour_primaries_name, preset_name, transfer_name},
};

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
    include!(concat!(env!("OUT_DIR"), "/x265.rs"));
}

const BACKEND: Backend = Backend::X265;
const VALIDATION_RESOLUTION: Resolution = Resolution::new(64, 64);

const LOW_LATENCY_DEFAULTS: [(&str, &str); 12] = [
    ("log-level", "warning"),
    ("info", "0"),
    ("bframes", "0"),
    ("rc-lookahead", "0"),
    ("frame-threads", "1"),
    ("scenecut", "0"),
    ("open-gop", "0"),
    ("keyint", "-1"),
    ("intra-refresh", "1"),
    ("repeat-headers", "1"),
    ("annexb", "1"),
    ("aud", "0"),
];

const fn tune_name(tune: X265Tune) -> &'static CStr {
    match tune {
        X265Tune::Zerolatency => c"zerolatency",
        X265Tune::Grain => c"grain",
        X265Tune::Animation => c"animation",
        X265Tune::Psnr => c"psnr",
        X265Tune::Ssim => c"ssim",
        X265Tune::Fastdecode => c"fastdecode",
    }
}

const fn input_csp_name(chroma: ChromaSubsampling) -> &'static str {
    match chroma {
        ChromaSubsampling::Yuv420 => "i420",
        ChromaSubsampling::Yuv422 => "i422",
        ChromaSubsampling::Yuv444 => "i444",
    }
}

const fn colour_space_id(chroma: ChromaSubsampling) -> c_int {
    match chroma {
        ChromaSubsampling::Yuv420 => sys::X265_CSP_I420,
        ChromaSubsampling::Yuv422 => sys::X265_CSP_I422,
        ChromaSubsampling::Yuv444 => sys::X265_CSP_I444,
    }
}

const fn range_name(range: ColorRange) -> &'static str {
    match range {
        ColorRange::Limited => "limited",
        ColorRange::Full => "full",
    }
}

fn kbps_to_int(kbps: u32) -> Result<c_int, EncodeError> {
    c_int::try_from(kbps).map_err(|_| EncodeError::BitrateRejected {
        backend: BACKEND,
        kbps,
    })
}

#[derive(Clone, Copy)]
struct Api {
    param_alloc: unsafe extern "C" fn() -> *mut sys::x265_param,
    param_free: unsafe extern "C" fn(*mut sys::x265_param),
    param_parse: unsafe extern "C" fn(*mut sys::x265_param, *const c_char, *const c_char) -> c_int,
    param_default_preset:
        unsafe extern "C" fn(*mut sys::x265_param, *const c_char, *const c_char) -> c_int,
    picture_alloc: unsafe extern "C" fn() -> *mut sys::x265_picture,
    picture_free: unsafe extern "C" fn(*mut sys::x265_picture),
    picture_init: unsafe extern "C" fn(*mut sys::x265_param, *mut sys::x265_picture),
    encoder_open: unsafe extern "C" fn(*mut sys::x265_param) -> *mut sys::x265_encoder,
    encoder_parameters: unsafe extern "C" fn(*mut sys::x265_encoder, *mut sys::x265_param),
    encoder_reconfig: unsafe extern "C" fn(*mut sys::x265_encoder, *mut sys::x265_param) -> c_int,
    encoder_encode: unsafe extern "C" fn(
        *mut sys::x265_encoder,
        *mut *mut sys::x265_nal,
        *mut u32,
        *mut sys::x265_picture,
        *mut sys::x265_picture,
    ) -> c_int,
    encoder_close: unsafe extern "C" fn(*mut sys::x265_encoder),
}

impl Api {
    fn load(depth: BitDepth) -> Result<Self, EncodeError> {
        let incompatible = EncodeError::LibraryIncompatible { backend: BACKEND };

        let mut status: c_int = 0;
        let raw = unsafe {
            sys::x265_api_query(c_int::from(depth.bits()), sys::X265_BUILD, &raw mut status)
        };
        let raw = unsafe { raw.as_ref() }.ok_or(EncodeError::BitDepthUnsupported {
            backend: BACKEND,
            depth,
        })?;

        let sizes_match = usize::try_from(raw.sizeof_param)
            .is_ok_and(|size| size == mem::size_of::<sys::x265_param>())
            && usize::try_from(raw.sizeof_picture)
                .is_ok_and(|size| size == mem::size_of::<sys::x265_picture>());
        if !sizes_match {
            return Err(incompatible);
        }

        match (
            raw.param_alloc,
            raw.param_free,
            raw.param_parse,
            raw.param_default_preset,
            raw.picture_alloc,
            raw.picture_free,
            raw.picture_init,
            raw.encoder_open,
            raw.encoder_parameters,
            raw.encoder_reconfig,
            raw.encoder_encode,
            raw.encoder_close,
        ) {
            (
                Some(param_alloc),
                Some(param_free),
                Some(param_parse),
                Some(param_default_preset),
                Some(picture_alloc),
                Some(picture_free),
                Some(picture_init),
                Some(encoder_open),
                Some(encoder_parameters),
                Some(encoder_reconfig),
                Some(encoder_encode),
                Some(encoder_close),
            ) => Ok(Self {
                param_alloc,
                param_free,
                param_parse,
                param_default_preset,
                picture_alloc,
                picture_free,
                picture_init,
                encoder_open,
                encoder_parameters,
                encoder_reconfig,
                encoder_encode,
                encoder_close,
            }),
            _ => Err(incompatible),
        }
    }
}

struct Param {
    api: Api,
    raw: NonNull<sys::x265_param>,
}

impl Drop for Param {
    fn drop(&mut self) {
        unsafe { (self.api.param_free)(self.raw.as_ptr()) };
    }
}

impl Param {
    fn alloc(api: Api) -> Result<Self, EncodeError> {
        let raw = NonNull::new(unsafe { (api.param_alloc)() })
            .ok_or(EncodeError::LibraryIncompatible { backend: BACKEND })?;

        Ok(Self { api, raw })
    }

    fn parse(&mut self, key: &str, value: &str) -> Result<(), EncodeError> {
        let pair = CParam::new(BACKEND, key, value)?;

        let status = unsafe {
            (self.api.param_parse)(self.raw.as_ptr(), pair.key.as_ptr(), pair.value.as_ptr())
        };

        match status {
            0 => Ok(()),
            sys::X265_PARAM_BAD_NAME => Err(EncodeError::UnknownParameter {
                backend: BACKEND,
                key: key.to_owned(),
            }),
            _ => Err(EncodeError::InvalidParameterValue {
                backend: BACKEND,
                key: key.to_owned(),
                value: value.to_owned(),
            }),
        }
    }

    fn rate(&self) -> (c_int, c_int, c_int) {
        let param = unsafe { self.raw.as_ref() };

        (
            param.rc.bitrate,
            param.rc.vbvMaxBitrate,
            param.rc.vbvBufferSize,
        )
    }
}

struct Picture {
    api: Api,
    raw: NonNull<sys::x265_picture>,
}

impl Drop for Picture {
    fn drop(&mut self) {
        unsafe { (self.api.picture_free)(self.raw.as_ptr()) };
    }
}

impl Picture {
    fn alloc(api: Api, param: &Param) -> Result<Self, EncodeError> {
        let raw = NonNull::new(unsafe { (api.picture_alloc)() })
            .ok_or(EncodeError::LibraryIncompatible { backend: BACKEND })?;

        unsafe { (api.picture_init)(param.raw.as_ptr(), raw.as_ptr()) };

        Ok(Self { api, raw })
    }
}

struct Settings {
    config: X265Config,
    pixel_format: PixelFormat,
    color: ColorSpace,
    frame_rate: FrameRate,
    bitrate_kbps: u32,
}

impl Settings {
    fn new(video: &VideoConfig, config: &X265Config) -> Result<Self, EncodeError> {
        let settings = Self {
            config: config.clone(),
            pixel_format: video.pixel_format,
            color: video.color,
            frame_rate: video.frame_rate,
            bitrate_kbps: video.bitrate_kbps,
        };

        let api = Api::load(settings.pixel_format.depth)?;
        settings.param(api, VALIDATION_RESOLUTION)?;

        Ok(settings)
    }

    fn param(&self, api: Api, resolution: Resolution) -> Result<Param, EncodeError> {
        let mut param = Param::alloc(api)?;

        let tune = self.config.tune.map(tune_name);
        let status = unsafe {
            (api.param_default_preset)(
                param.raw.as_ptr(),
                preset_name(self.config.preset).as_ptr(),
                tune.map_or(ptr::null(), CStr::as_ptr),
            )
        };
        if status != 0 {
            return Err(EncodeError::TuneRejected {
                backend: BACKEND,
                tune: tune.map_or_else(String::new, |tune| tune.to_string_lossy().into_owned()),
            });
        }

        for (key, value) in LOW_LATENCY_DEFAULTS {
            param.parse(key, value)?;
        }
        let bitrate = self.bitrate_kbps.to_string();
        let buffer = vbv_buffer_kbit(self.bitrate_kbps, self.frame_rate).to_string();
        param.parse("vbv-maxrate", &bitrate)?;
        param.parse("vbv-bufsize", &buffer)?;

        for (key, value) in &self.config.params {
            param.parse(key, value)?;
        }

        self.apply_typed(&mut param, resolution)?;

        Ok(param)
    }

    fn apply_typed(&self, param: &mut Param, resolution: Resolution) -> Result<(), EncodeError> {
        param.parse(
            "input-res",
            &format!("{}x{}", resolution.width, resolution.height),
        )?;
        param.parse(
            "fps",
            &format!(
                "{}/{}",
                self.frame_rate.numerator, self.frame_rate.denominator
            ),
        )?;
        param.parse("input-csp", input_csp_name(self.pixel_format.chroma))?;
        param.parse("bitrate", &self.bitrate_kbps.to_string())?;

        if let Some(threads) = self.config.threads {
            param.parse("pools", &threads.get().to_string())?;
        }

        param.parse("colorprim", colour_primaries_name(self.color.primaries))?;
        param.parse("colormatrix", colour_matrix_name(self.color.primaries))?;
        param.parse("transfer", transfer_name(self.color.transfer))?;
        param.parse("range", range_name(self.color.range))
    }
}

struct Session {
    api: Api,
    handle: NonNull<sys::x265_encoder>,
    param: Param,
    input: Picture,
    output: Picture,
    resolution: Resolution,
    colour_space: c_int,
    bit_depth: c_int,
    frame_rate: FrameRate,
}

unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { (self.api.encoder_close)(self.handle.as_ptr()) };
    }
}

impl Session {
    fn open(settings: &Settings, resolution: Resolution) -> Result<Self, EncodeError> {
        let api = Api::load(settings.pixel_format.depth)?;
        let param = settings.param(api, resolution)?;

        let handle = NonNull::new(unsafe { (api.encoder_open)(param.raw.as_ptr()) }).ok_or(
            EncodeError::OpenFailed {
                backend: BACKEND,
                resolution,
            },
        )?;

        let input = Picture::alloc(api, &param)?;
        let output = Picture::alloc(api, &param)?;

        Ok(Self {
            api,
            handle,
            param,
            input,
            output,
            resolution,
            colour_space: colour_space_id(settings.pixel_format.chroma),
            bit_depth: c_int::from(settings.pixel_format.depth.bits()),
            frame_rate: settings.frame_rate,
        })
    }

    fn encode(
        &mut self,
        view: &YuvFrame<'_>,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, EncodeError> {
        let input = unsafe { self.input.raw.as_mut() };

        input.pts = i64::try_from(timestamp.as_micros()).unwrap_or(i64::MAX);
        input.bitDepth = self.bit_depth;
        input.colorSpace = self.colour_space;
        input.sliceType = if keyframe {
            sys::X265_TYPE_IDR
        } else {
            sys::X265_TYPE_AUTO
        };

        for (index, plane) in view.planes.iter().enumerate() {
            input.planes[index] = plane.data.as_ptr().cast_mut().cast::<c_void>();
            input.stride[index] = c_int::try_from(plane.stride).unwrap_or(c_int::MAX);
        }

        let mut nals: *mut sys::x265_nal = ptr::null_mut();
        let mut nal_count: u32 = 0;

        let status = unsafe {
            (self.api.encoder_encode)(
                self.handle.as_ptr(),
                &raw mut nals,
                &raw mut nal_count,
                self.input.raw.as_ptr(),
                self.output.raw.as_ptr(),
            )
        };

        if status < 0 {
            return Err(EncodeError::EncodeFailed {
                backend: BACKEND,
                code: status,
            });
        }

        let Some(first) = NonNull::new(nals).filter(|_| status > 0 && nal_count > 0) else {
            return Ok(None);
        };

        let units = unsafe { slice::from_raw_parts(first.as_ptr(), nal_count as usize) };
        let mut data = Vec::with_capacity(units.iter().map(|unit| unit.sizeBytes as usize).sum());
        for unit in units {
            data.extend_from_slice(unsafe {
                slice::from_raw_parts(unit.payload, unit.sizeBytes as usize)
            });
        }

        let output = unsafe { self.output.raw.as_ref() };

        Ok(Some(EncodedFrame {
            codec: Codec::Hevc,
            timestamp: Timestamp::from_micros(u64::try_from(output.pts).unwrap_or(0)),
            keyframe: output.sliceType == sys::X265_TYPE_IDR,
            data,
        }))
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), EncodeError> {
        let rejected = || EncodeError::BitrateRejected {
            backend: BACKEND,
            kbps,
        };

        if kbps == 0 || kbps_to_int(kbps).is_err() {
            return Err(rejected());
        }

        unsafe { (self.api.encoder_parameters)(self.handle.as_ptr(), self.param.raw.as_ptr()) };

        let (previous, maximum, buffer) = self.param.rate();
        let previous = i64::from(previous).max(1);
        let scale = |value: c_int| {
            let scaled = i64::from(value) * i64::from(kbps) / previous;
            scaled.max(1).to_string()
        };

        let maximum = if maximum > 0 {
            scale(maximum)
        } else {
            kbps.to_string()
        };
        let buffer = if buffer > 0 {
            scale(buffer)
        } else {
            vbv_buffer_kbit(kbps, self.frame_rate).to_string()
        };

        self.param.parse("bitrate", &kbps.to_string())?;
        self.param.parse("vbv-maxrate", &maximum)?;
        self.param.parse("vbv-bufsize", &buffer)?;

        let status =
            unsafe { (self.api.encoder_reconfig)(self.handle.as_ptr(), self.param.raw.as_ptr()) };

        if status < 0 { Err(rejected()) } else { Ok(()) }
    }
}

pub struct X265Encoder {
    settings: Settings,
    session: Option<Session>,
    keyframe_pending: bool,
}

impl X265Encoder {
    pub fn new(video: &VideoConfig, config: &X265Config) -> Result<Self, EncodeError> {
        let settings = Settings::new(video, config)?;

        let session = video
            .resolution
            .map(|resolution| Session::open(&settings, resolution))
            .transpose()?;

        Ok(Self {
            settings,
            session,
            keyframe_pending: false,
        })
    }
}

impl VideoEncoder for X265Encoder {
    fn encode(&mut self, frame: &Frame) -> Result<Option<EncodedFrame>, EncodeError> {
        let view = YuvFrame::from_frame(frame, BACKEND, self.settings.pixel_format)?;

        let session = match &mut self.session {
            Some(session) => session,
            slot => slot.insert(Session::open(&self.settings, frame.resolution)?),
        };

        if session.resolution != frame.resolution {
            return Err(EncodeError::FrameResolutionMismatch {
                backend: BACKEND,
                expected: session.resolution,
                actual: frame.resolution,
            });
        }

        let encoded = session.encode(&view, frame.timestamp, self.keyframe_pending)?;
        self.keyframe_pending = false;

        Ok(encoded)
    }

    fn request_keyframe(&mut self) {
        self.keyframe_pending = true;
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), EncodeError> {
        match &mut self.session {
            Some(session) => session.set_bitrate(kbps)?,
            None if kbps == 0 => {
                return Err(EncodeError::BitrateRejected {
                    backend: BACKEND,
                    kbps,
                });
            }
            None => {}
        }

        self.settings.bitrate_kbps = kbps;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rusplay_config::VideoEncoderConfig;

    use super::*;
    use crate::testing::{self, pixel, start_code_offsets, video};

    const IDR_W_RADL: u8 = 19;
    const IDR_N_LP: u8 = 20;

    fn contains_idr(data: &[u8]) -> bool {
        start_code_offsets(data).into_iter().any(|offset| {
            data.get(offset)
                .is_some_and(|byte| matches!((byte >> 1) & 0x3f, IDR_W_RADL | IDR_N_LP))
        })
    }

    fn config(format: PixelFormat, x265: X265Config) -> VideoConfig {
        video(Codec::Hevc, format, VideoEncoderConfig::X265(x265))
    }

    fn with_params(pairs: &[(&str, &str)]) -> VideoConfig {
        config(
            PixelFormat::default(),
            X265Config {
                params: testing::params(pairs),
                ..X265Config::default()
            },
        )
    }

    #[test]
    fn stream_starts_on_an_idr_for_every_chroma_and_depth() {
        for format in [
            pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            pixel(ChromaSubsampling::Yuv422, BitDepth::Ten),
            pixel(ChromaSubsampling::Yuv444, BitDepth::Eight),
            pixel(ChromaSubsampling::Yuv444, BitDepth::Twelve),
        ] {
            testing::check_stream_starts_on_a_keyframe(
                &config(format, X265Config::default()),
                contains_idr,
            );
        }
    }

    #[test]
    fn requested_keyframe_is_an_idr() {
        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);

        testing::check_keyframe_on_request(&config(format, X265Config::default()), contains_idr);
    }

    #[test]
    fn set_bitrate_reconfigures_in_place() {
        testing::check_set_bitrate_in_place(&config(PixelFormat::default(), X265Config::default()));
    }

    #[test]
    fn unknown_parameter_names_the_key() {
        testing::check_unknown_parameter_names_the_key(&with_params(&[("no-such-knob", "1")]));
    }

    #[test]
    fn invalid_parameter_value_names_the_key() {
        testing::check_invalid_value_names_the_key(
            &with_params(&[("bframes", "plenty")]),
            "bframes",
            "plenty",
        );
    }

    #[test]
    fn passthrough_parameters_reach_the_library() {
        let config = with_params(&[("aq-mode", "3"), ("bframes", "2")]);
        let VideoEncoderConfig::X265(x265) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, x265).unwrap();
        let api = Api::load(config.pixel_format.depth).unwrap();

        let param = settings.param(api, testing::TEST_RESOLUTION).unwrap();
        let raw = unsafe { param.raw.as_ref() };

        assert_eq!(raw.rc.aqMode, 3);
        assert_eq!(raw.bframes, 2);
    }

    #[test]
    fn typed_fields_win_over_params() {
        let config = with_params(&[("bitrate", "1"), ("fps", "1/1")]);
        let VideoEncoderConfig::X265(x265) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, x265).unwrap();
        let api = Api::load(config.pixel_format.depth).unwrap();

        let param = settings.param(api, testing::TEST_RESOLUTION).unwrap();
        let raw = unsafe { param.raw.as_ref() };

        assert_eq!(raw.rc.bitrate, 8_000);
        assert_eq!(raw.fpsNum, 60);
    }

    #[test]
    fn wrong_frame_inputs_are_rejected() {
        testing::check_wrong_inputs_are_rejected(&config(
            pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            X265Config::default(),
        ));
    }

    #[test]
    fn unspecified_resolution_follows_the_first_frame() {
        testing::check_unspecified_resolution_follows_the_first_frame(&config(
            PixelFormat::default(),
            X265Config::default(),
        ));
    }
}
