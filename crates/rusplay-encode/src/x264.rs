use std::{
    collections::VecDeque,
    ffi::{CString, c_int},
    ptr::{self, NonNull},
    slice,
};

use rusplay_capture::Frame;
use rusplay_config::{VideoConfig, X264Config, X264Tune};
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
    include!(concat!(env!("OUT_DIR"), "/x264.rs"));
}

const BACKEND: Backend = Backend::X264;
const VALIDATION_RESOLUTION: Resolution = Resolution::new(64, 64);

const LOW_LATENCY_DEFAULTS: [(&str, &str); 9] = [
    ("bframes", "0"),
    ("rc-lookahead", "0"),
    ("sync-lookahead", "0"),
    ("sliced-threads", "1"),
    ("scenecut", "0"),
    ("keyint", "infinite"),
    ("intra-refresh", "1"),
    ("repeat-headers", "1"),
    ("aud", "0"),
];

pub(crate) fn supports(format: PixelFormat) -> bool {
    format.depth != BitDepth::Twelve
}

const fn tune_name(tune: X264Tune) -> &'static str {
    match tune {
        X264Tune::Zerolatency => "zerolatency",
        X264Tune::Film => "film",
        X264Tune::Animation => "animation",
        X264Tune::Grain => "grain",
        X264Tune::Stillimage => "stillimage",
        X264Tune::Psnr => "psnr",
        X264Tune::Ssim => "ssim",
        X264Tune::Fastdecode => "fastdecode",
    }
}

const fn full_range_flag(range: ColorRange) -> c_int {
    match range {
        ColorRange::Limited => 0,
        ColorRange::Full => 1,
    }
}

const fn colour_space_id(chroma: ChromaSubsampling) -> c_int {
    match chroma {
        ChromaSubsampling::Yuv420 => sys::X264_CSP_I420,
        ChromaSubsampling::Yuv422 => sys::X264_CSP_I422,
        ChromaSubsampling::Yuv444 => sys::X264_CSP_I444,
    }
}

fn parse(param: &mut sys::x264_param_t, key: &str, value: &str) -> Result<(), EncodeError> {
    let pair = CParam::new(BACKEND, key, value)?;

    let status = unsafe { sys::x264_param_parse(param, pair.key.as_ptr(), pair.value.as_ptr()) };

    match status {
        0 => Ok(()),
        sys::X264_PARAM_BAD_NAME => Err(EncodeError::UnknownParameter {
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

fn dimension(value: u32, resolution: Resolution) -> Result<c_int, EncodeError> {
    c_int::try_from(value).map_err(|_| EncodeError::OpenFailed {
        backend: BACKEND,
        resolution,
    })
}

fn kbps_to_int(kbps: u32) -> Result<c_int, EncodeError> {
    c_int::try_from(kbps).map_err(|_| EncodeError::BitrateRejected {
        backend: BACKEND,
        kbps,
    })
}

struct Settings {
    config: X264Config,
    pixel_format: PixelFormat,
    color: ColorSpace,
    frame_rate: FrameRate,
    bitrate_kbps: u32,
}

impl Settings {
    fn new(video: &VideoConfig, config: &X264Config) -> Result<Self, EncodeError> {
        if !supports(video.pixel_format) {
            return Err(EncodeError::BitDepthUnsupported {
                backend: BACKEND,
                depth: video.pixel_format.depth,
            });
        }

        let settings = Self {
            config: config.clone(),
            pixel_format: video.pixel_format,
            color: video.color,
            frame_rate: video.frame_rate,
            bitrate_kbps: video.bitrate_kbps,
        };

        settings.param(VALIDATION_RESOLUTION)?;

        Ok(settings)
    }

    fn tune_list(&self) -> String {
        self.config
            .tune
            .iter()
            .map(|&tune| tune_name(tune))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn param(&self, resolution: Resolution) -> Result<sys::x264_param_t, EncodeError> {
        let mut param = sys::x264_param_t::default();

        let tune = self.tune_list();
        let tune_c = CString::new(tune.as_str()).map_err(|_| EncodeError::TuneRejected {
            backend: BACKEND,
            tune: tune.clone(),
        })?;
        let tune_ptr = if tune.is_empty() {
            ptr::null()
        } else {
            tune_c.as_ptr()
        };

        let status = unsafe {
            sys::x264_param_default_preset(
                &raw mut param,
                preset_name(self.config.preset).as_ptr(),
                tune_ptr,
            )
        };
        if status != 0 {
            return Err(EncodeError::TuneRejected {
                backend: BACKEND,
                tune,
            });
        }

        param.i_bitdepth = c_int::from(self.pixel_format.depth.bits());
        param.i_log_level = sys::X264_LOG_WARNING;

        for (key, value) in LOW_LATENCY_DEFAULTS {
            parse(&mut param, key, value)?;
        }
        let bitrate = self.bitrate_kbps.to_string();
        let buffer = vbv_buffer_kbit(self.bitrate_kbps, self.frame_rate).to_string();
        parse(&mut param, "bitrate", &bitrate)?;
        parse(&mut param, "vbv-maxrate", &bitrate)?;
        parse(&mut param, "vbv-bufsize", &buffer)?;

        for (key, value) in &self.config.params {
            parse(&mut param, key, value)?;
        }

        self.apply_typed(&mut param, resolution)?;

        Ok(param)
    }

    fn apply_typed(
        &self,
        param: &mut sys::x264_param_t,
        resolution: Resolution,
    ) -> Result<(), EncodeError> {
        param.i_width = dimension(resolution.width, resolution)?;
        param.i_height = dimension(resolution.height, resolution)?;
        param.i_csp = colour_space_id(self.pixel_format.chroma);
        param.i_bitdepth = c_int::from(self.pixel_format.depth.bits());
        param.i_fps_num = self.frame_rate.numerator;
        param.i_fps_den = self.frame_rate.denominator;
        param.b_vfr_input = 0;
        param.rc.i_bitrate = kbps_to_int(self.bitrate_kbps)?;

        if let Some(threads) = self.config.threads {
            param.i_threads = c_int::try_from(threads.get()).unwrap_or(c_int::MAX);
        }

        parse(
            param,
            "colorprim",
            colour_primaries_name(self.color.primaries),
        )?;
        parse(
            param,
            "colormatrix",
            colour_matrix_name(self.color.primaries),
        )?;
        parse(param, "transfer", transfer_name(self.color.transfer))?;
        param.vui.b_fullrange = full_range_flag(self.color.range);

        Ok(())
    }
}

#[derive(Default)]
struct TimestampLedger {
    next_pts: i64,
    issued: VecDeque<(i64, Timestamp)>,
}

impl TimestampLedger {
    fn issue(&mut self, timestamp: Timestamp) -> i64 {
        let pts = self.next_pts;
        self.next_pts += 1;
        self.issued.push_back((pts, timestamp));

        pts
    }

    fn resolve(&mut self, pts: i64) -> Option<Timestamp> {
        while self.issued.front().is_some_and(|&(issued, _)| issued < pts) {
            self.issued.pop_front();
        }

        match self.issued.pop_front() {
            Some((issued, timestamp)) if issued == pts => Some(timestamp),
            _ => None,
        }
    }
}

struct Session {
    handle: NonNull<sys::x264_t>,
    resolution: Resolution,
    image_csp: c_int,
    frame_rate: FrameRate,
    ledger: TimestampLedger,
}

unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe { sys::x264_encoder_close(self.handle.as_ptr()) };
    }
}

impl Session {
    fn open(settings: &Settings, resolution: Resolution) -> Result<Self, EncodeError> {
        let mut param = settings.param(resolution)?;

        let handle = NonNull::new(unsafe { sys::x264_encoder_open(&raw mut param) }).ok_or(
            EncodeError::OpenFailed {
                backend: BACKEND,
                resolution,
            },
        )?;

        let high_depth = if settings.pixel_format.depth == BitDepth::Eight {
            0
        } else {
            sys::X264_CSP_HIGH_DEPTH
        };

        Ok(Self {
            handle,
            resolution,
            image_csp: colour_space_id(settings.pixel_format.chroma) | high_depth,
            frame_rate: settings.frame_rate,
            ledger: TimestampLedger::default(),
        })
    }

    fn encode(
        &mut self,
        view: &YuvFrame<'_>,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, EncodeError> {
        let mut input = sys::x264_picture_t::default();
        unsafe { sys::x264_picture_init(&raw mut input) };

        input.i_type = if keyframe {
            sys::X264_TYPE_IDR
        } else {
            sys::X264_TYPE_AUTO
        };
        input.i_pts = self.ledger.issue(timestamp);
        input.img.i_csp = self.image_csp;
        input.img.i_plane = 3;

        for (index, plane) in view.planes.iter().enumerate() {
            input.img.plane[index] = plane.data.as_ptr().cast_mut();
            input.img.i_stride[index] = c_int::try_from(plane.stride).unwrap_or(c_int::MAX);
        }

        let mut output = sys::x264_picture_t::default();
        let mut nals: *mut sys::x264_nal_t = ptr::null_mut();
        let mut nal_count: c_int = 0;

        let size = unsafe {
            sys::x264_encoder_encode(
                self.handle.as_ptr(),
                &raw mut nals,
                &raw mut nal_count,
                &raw mut input,
                &raw mut output,
            )
        };

        if size < 0 {
            return Err(EncodeError::EncodeFailed {
                backend: BACKEND,
                code: size,
            });
        }

        let length = usize::try_from(size).unwrap_or(0);
        let Some(first) = NonNull::new(nals).filter(|_| nal_count > 0 && length > 0) else {
            return Ok(None);
        };

        let payload = unsafe { first.as_ref().p_payload };
        let data = unsafe { slice::from_raw_parts(payload, length) }.to_vec();

        Ok(Some(EncodedFrame {
            codec: Codec::H264,
            timestamp: self.ledger.resolve(output.i_pts).unwrap_or(timestamp),
            keyframe: output.b_keyframe != 0,
            data,
        }))
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), EncodeError> {
        let rejected = || EncodeError::BitrateRejected {
            backend: BACKEND,
            kbps,
        };

        if kbps == 0 {
            return Err(rejected());
        }

        let mut param = sys::x264_param_t::default();
        unsafe { sys::x264_encoder_parameters(self.handle.as_ptr(), &raw mut param) };

        let previous = i64::from(param.rc.i_bitrate).max(1);
        let scale = |value: c_int| {
            let scaled = i64::from(value) * i64::from(kbps) / previous;
            c_int::try_from(scaled.max(1)).unwrap_or(c_int::MAX)
        };

        let buffer = if param.rc.i_vbv_buffer_size > 0 {
            scale(param.rc.i_vbv_buffer_size)
        } else {
            c_int::try_from(vbv_buffer_kbit(kbps, self.frame_rate)).unwrap_or(c_int::MAX)
        };
        let maximum = if param.rc.i_vbv_max_bitrate > 0 {
            scale(param.rc.i_vbv_max_bitrate)
        } else {
            kbps_to_int(kbps)?
        };

        param.rc.i_bitrate = kbps_to_int(kbps)?;
        param.rc.i_vbv_max_bitrate = maximum;
        param.rc.i_vbv_buffer_size = buffer;

        let status = unsafe { sys::x264_encoder_reconfig(self.handle.as_ptr(), &raw mut param) };

        if status < 0 { Err(rejected()) } else { Ok(()) }
    }
}

pub struct X264Encoder {
    settings: Settings,
    session: Option<Session>,
    keyframe_pending: bool,
}

impl X264Encoder {
    pub fn new(video: &VideoConfig, config: &X264Config) -> Result<Self, EncodeError> {
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

impl VideoEncoder for X264Encoder {
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

    const IDR_NAL_TYPE: u8 = 5;

    fn contains_idr(data: &[u8]) -> bool {
        start_code_offsets(data).into_iter().any(|offset| {
            data.get(offset)
                .is_some_and(|byte| byte & 0x1f == IDR_NAL_TYPE)
        })
    }

    fn config(format: PixelFormat, x264: X264Config) -> VideoConfig {
        video(Codec::H264, format, VideoEncoderConfig::X264(x264))
    }

    fn with_params(pairs: &[(&str, &str)]) -> VideoConfig {
        config(
            PixelFormat::default(),
            X264Config {
                params: testing::params(pairs),
                ..X264Config::default()
            },
        )
    }

    #[test]
    fn stream_starts_on_an_idr_for_every_chroma_and_depth() {
        for format in [
            pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            pixel(ChromaSubsampling::Yuv422, BitDepth::Ten),
            pixel(ChromaSubsampling::Yuv444, BitDepth::Eight),
            pixel(ChromaSubsampling::Yuv444, BitDepth::Ten),
        ] {
            testing::check_stream_starts_on_a_keyframe(
                &config(format, X264Config::default()),
                contains_idr,
            );
        }
    }

    #[test]
    fn requested_keyframe_is_an_idr() {
        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);

        testing::check_keyframe_on_request(&config(format, X264Config::default()), contains_idr);
    }

    #[test]
    fn set_bitrate_reconfigures_in_place() {
        testing::check_set_bitrate_in_place(&config(PixelFormat::default(), X264Config::default()));
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
        let VideoEncoderConfig::X264(x264) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, x264).unwrap();

        let param = settings.param(testing::TEST_RESOLUTION).unwrap();

        assert_eq!(param.rc.i_aq_mode, 3);
        assert_eq!(param.i_bframe, 2);
    }

    #[test]
    fn typed_fields_win_over_params() {
        let config = with_params(&[("bitrate", "1"), ("fps", "1/1")]);
        let VideoEncoderConfig::X264(x264) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, x264).unwrap();

        let param = settings.param(testing::TEST_RESOLUTION).unwrap();

        assert_eq!(param.rc.i_bitrate, 8_000);
        assert_eq!(param.i_fps_num, 60);
    }

    #[test]
    fn twelve_bit_is_unsupported() {
        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Twelve);

        let error = crate::open(&config(format, X264Config::default()))
            .err()
            .unwrap();

        assert!(matches!(error, EncodeError::BitDepthUnsupported { .. }));
    }

    #[test]
    fn wrong_frame_inputs_are_rejected() {
        testing::check_wrong_inputs_are_rejected(&config(
            pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            X264Config::default(),
        ));
    }

    #[test]
    fn unspecified_resolution_follows_the_first_frame() {
        testing::check_unspecified_resolution_follows_the_first_frame(&config(
            PixelFormat::default(),
            X264Config::default(),
        ));
    }

    #[test]
    fn ledger_resolves_issued_timestamps_in_order() {
        let mut ledger = TimestampLedger::default();
        let first = ledger.issue(Timestamp::from_micros(10));
        let second = ledger.issue(Timestamp::from_micros(20));

        assert_eq!(ledger.resolve(first), Some(Timestamp::from_micros(10)));
        assert_eq!(ledger.resolve(second), Some(Timestamp::from_micros(20)));
        assert_eq!(ledger.resolve(second), None);
    }
}
