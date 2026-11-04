use std::{
    collections::VecDeque,
    ffi::{CString, c_int},
    ptr::{self, NonNull},
    slice,
};

use rusplay_config::{VideoConfig, X264Config, X264Tune};
use rusplay_core::{
    BitDepth, ChromaSiting, ChromaSubsampling, Codec, ColorRange, ColorSpace, EncodedFrame, Frame,
    FrameRate, HdrLevels, PixelFormat, Resolution, Timestamp,
};

use crate::{
    Backend, EncodeError, OutputIndex, Recovery, VideoEncoder,
    hdr::St2086,
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
const RECOVERY_DPB_FRAMES: u64 = 8;
const MACROBLOCK_SIZE: u32 = 16;
const DEFAULT_LEVEL_DPB_MACROBLOCKS: u64 = 184_320;

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

const fn level_dpb_macroblocks(level_idc: c_int) -> u64 {
    match level_idc {
        ..=0 | 51 | 52 => DEFAULT_LEVEL_DPB_MACROBLOCKS,
        1..=10 => 396,
        11 => 900,
        12..=20 => 2_376,
        21 => 4_752,
        22..=30 => 8_100,
        31 => 18_000,
        32 => 20_480,
        40 | 41 => 32_768,
        42 => 34_816,
        50 => 110_400,
        _ => 696_320,
    }
}

fn recovery_frames(param: &sys::x264_param_t, resolution: Resolution) -> Option<u64> {
    let frame_threaded = param.b_sliced_threads == 0 && param.i_threads != 1;
    let held_back = param.i_bframe != 0 || param.rc.i_lookahead != 0 || param.i_sync_lookahead != 0;

    if param.b_intra_refresh != 0 || held_back || frame_threaded {
        return None;
    }

    let macroblocks = u64::from(resolution.width.div_ceil(MACROBLOCK_SIZE))
        * u64::from(resolution.height.div_ceil(MACROBLOCK_SIZE));
    let level_frames = level_dpb_macroblocks(param.i_level_idc) / macroblocks.max(1);
    let references = u64::try_from(param.i_frame_reference).unwrap_or(1);
    let frames = RECOVERY_DPB_FRAMES.min(level_frames).max(references);

    (frames > 1).then_some(frames)
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

fn chroma_location(siting: ChromaSiting) -> c_int {
    c_int::try_from(siting.vui_chroma_location()).unwrap_or_default()
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
    siting: ChromaSiting,
    frame_rate: FrameRate,
    vbv_frames: u8,
    bitrate_kbps: u32,
}

impl Settings {
    fn new(
        video: &VideoConfig,
        config: &X264Config,
        siting: ChromaSiting,
    ) -> Result<Self, EncodeError> {
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
            siting,
            frame_rate: video.frame_rate,
            vbv_frames: video.vbv_frames,
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
        let buffer =
            vbv_buffer_kbit(self.bitrate_kbps, self.frame_rate, self.vbv_frames).to_string();
        parse(&mut param, "bitrate", &bitrate)?;
        parse(&mut param, "vbv-maxrate", &bitrate)?;
        parse(&mut param, "vbv-bufsize", &buffer)?;

        for (key, value) in &self.config.params {
            parse(&mut param, key, value)?;
        }

        self.apply_typed(&mut param, resolution)?;

        if let Some(frames) = recovery_frames(&param, resolution) {
            param.i_dpb_size = param
                .i_dpb_size
                .max(c_int::try_from(frames).unwrap_or(c_int::MAX));
        }

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
            colour_primaries_name(BACKEND, self.color.primaries)?,
        )?;
        parse(
            param,
            "colormatrix",
            colour_matrix_name(self.color.matrix_coefficients()),
        )?;
        parse(param, "transfer", transfer_name(self.color.transfer))?;
        param.vui.b_fullrange = full_range_flag(self.color.range);
        param.vui.i_chroma_loc = chroma_location(self.siting);
        apply_hdr(param, &self.color.hdr);

        Ok(())
    }
}

fn apply_hdr(param: &mut sys::x264_param_t, hdr: &HdrLevels) {
    let point = |[x, y]: [u16; 2]| (c_int::from(x), c_int::from(y));

    if let Some(display) = &hdr.mastering {
        let units = St2086::of(display);
        let target = &mut param.mastering_display;
        target.b_mastering_display = 1;
        (target.i_green_x, target.i_green_y) = point(units.green);
        (target.i_blue_x, target.i_blue_y) = point(units.blue);
        (target.i_red_x, target.i_red_y) = point(units.red);
        (target.i_white_x, target.i_white_y) = point(units.white);
        target.i_display_max = i64::from(units.max_luminance);
        target.i_display_min = i64::from(units.min_luminance);
    }
    if let Some(light) = &hdr.content_light {
        let target = &mut param.content_light_level;
        target.b_cll = 1;
        target.i_max_cll = c_int::from(light.max_cll_nits);
        target.i_max_fall = c_int::from(light.max_fall_nits);
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

struct ReferenceWindow {
    frames: u64,
    produced: u64,
    keyframe: u64,
    pts: VecDeque<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forget {
    Nothing,
    From(i64),
}

impl ReferenceWindow {
    fn new(frames: u64) -> Self {
        Self {
            frames,
            produced: 0,
            keyframe: 0,
            pts: VecDeque::new(),
        }
    }

    fn record(&mut self, pts: i64, keyframe: bool) {
        if keyframe {
            self.keyframe = self.produced;
        }

        self.pts.push_back(pts);
        while u64::try_from(self.pts.len()).unwrap_or(u64::MAX) > self.frames {
            self.pts.pop_front();
        }
        self.produced += 1;
    }

    fn plan(&self, last_good: OutputIndex) -> Option<Forget> {
        let oldest = self.produced.saturating_sub(self.frames).max(self.keyframe);
        let held = (oldest..self.produced).contains(&last_good.0);

        if !held {
            return None;
        }
        if last_good.0 + 1 == self.produced {
            return Some(Forget::Nothing);
        }

        let first = self.produced - u64::try_from(self.pts.len()).unwrap_or(0);
        let offset = usize::try_from(last_good.0 + 1 - first).ok()?;

        self.pts.get(offset).copied().map(Forget::From)
    }
}

struct Session {
    handle: NonNull<sys::x264_t>,
    window: Option<ReferenceWindow>,
    resolution: Resolution,
    image_csp: c_int,
    frame_rate: FrameRate,
    vbv_frames: u8,
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
        let window = recovery_frames(&param, resolution).map(ReferenceWindow::new);

        Ok(Self {
            handle,
            window,
            resolution,
            image_csp: colour_space_id(settings.pixel_format.chroma) | high_depth,
            frame_rate: settings.frame_rate,
            vbv_frames: settings.vbv_frames,
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
        let keyframe = output.b_keyframe != 0;

        if let Some(window) = &mut self.window {
            window.record(output.i_pts, keyframe);
        }

        Ok(Some(EncodedFrame {
            codec: Codec::H264,
            timestamp: self.ledger.resolve(output.i_pts).unwrap_or(timestamp),
            keyframe,
            data: data.into(),
        }))
    }

    fn recover(&mut self, last_good: OutputIndex) -> Option<OutputIndex> {
        match self.window.as_ref()?.plan(last_good)? {
            Forget::Nothing => {}
            Forget::From(pts) => {
                let status =
                    unsafe { sys::x264_encoder_invalidate_reference(self.handle.as_ptr(), pts) };
                if status < 0 {
                    return None;
                }
            }
        }

        Some(last_good)
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
            c_int::try_from(vbv_buffer_kbit(kbps, self.frame_rate, self.vbv_frames))
                .unwrap_or(c_int::MAX)
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
    pub fn new(
        video: &VideoConfig,
        config: &X264Config,
        siting: ChromaSiting,
    ) -> Result<Self, EncodeError> {
        let settings = Settings::new(video, config, siting)?;

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

    fn recover(&mut self, last_good: OutputIndex) -> Recovery {
        let reference = match &mut self.session {
            Some(session) if !self.keyframe_pending => session.recover(last_good),
            _ => None,
        };

        if let Some(reference) = reference {
            return Recovery::Reference(reference);
        }

        self.request_keyframe();

        Recovery::Keyframe
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
    use rusplay_config::{VaapiDecoderConfig, VideoEncoderConfig};

    use super::*;
    use crate::{
        hdr::fixtures::hdr10_color,
        testing::{self, pixel, start_code_offsets, video},
    };

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

    fn without_intra_refresh(format: PixelFormat) -> VideoConfig {
        config(
            format,
            X264Config {
                params: testing::params(&[("intra-refresh", "0")]),
                ..X264Config::default()
            },
        )
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
    fn recovery_under_intra_refresh_is_an_idr() {
        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);

        testing::check_recovery_falls_back_to_a_keyframe(
            &config(format, X264Config::default()),
            contains_idr,
        );
    }

    #[test]
    fn recovery_is_offered_without_intra_refresh_and_b_frames() {
        let config = without_intra_refresh(PixelFormat::default());
        let VideoEncoderConfig::X264(x264) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, x264, ChromaSiting::default()).unwrap();

        let param = settings.param(testing::TEST_RESOLUTION).unwrap();

        assert_eq!(
            recovery_frames(&param, testing::TEST_RESOLUTION),
            Some(RECOVERY_DPB_FRAMES)
        );
        assert_eq!(u64::try_from(param.i_dpb_size), Ok(RECOVERY_DPB_FRAMES));
    }

    #[test]
    fn the_dpb_stays_within_the_level_limit() {
        let param = sys::x264_param_t {
            i_level_idc: 51,
            i_frame_reference: 1,
            b_sliced_threads: 1,
            ..sys::x264_param_t::default()
        };

        let uhd = Resolution::new(3840, 2160);

        assert_eq!(recovery_frames(&param, uhd), Some(5));
        assert_eq!(recovery_frames(&param, Resolution::new(7680, 4320)), None);
    }

    #[test]
    fn frame_threading_and_lookahead_withhold_recovery() {
        let mut param = sys::x264_param_t {
            i_frame_reference: 3,
            b_sliced_threads: 1,
            ..sys::x264_param_t::default()
        };

        assert_eq!(recovery_frames(&param, testing::TEST_RESOLUTION), Some(8));

        param.rc.i_lookahead = 10;
        assert_eq!(recovery_frames(&param, testing::TEST_RESOLUTION), None);

        param.rc.i_lookahead = 0;
        param.i_bframe = 2;
        assert_eq!(recovery_frames(&param, testing::TEST_RESOLUTION), None);

        param.i_bframe = 0;
        param.b_sliced_threads = 0;
        param.i_threads = 4;
        assert_eq!(recovery_frames(&param, testing::TEST_RESOLUTION), None);
    }

    #[test]
    fn a_loss_older_than_the_window_is_an_idr() {
        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let mut encoder = crate::open(&without_intra_refresh(format)).unwrap();

        for index in 0..20 {
            encoder.encode(&testing::frame(format, index)).unwrap();
        }
        let stale = encoder.recover(OutputIndex(5));
        let recovered = encoder
            .encode(&testing::frame(format, 20))
            .unwrap()
            .unwrap();

        assert_eq!(stale, Recovery::Keyframe);
        assert!(recovered.keyframe);
        assert!(contains_idr(&recovered.data));
    }

    #[test]
    fn a_loss_before_the_last_keyframe_is_an_idr() {
        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let mut encoder = crate::open(&without_intra_refresh(format)).unwrap();

        for index in 0..4 {
            encoder.encode(&testing::frame(format, index)).unwrap();
        }
        encoder.request_keyframe();
        for index in 4..8 {
            encoder.encode(&testing::frame(format, index)).unwrap();
        }

        assert_eq!(encoder.recover(OutputIndex(2)), Recovery::Keyframe);
    }

    #[test]
    fn a_recovery_names_the_last_good_output() {
        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let mut encoder = crate::open(&without_intra_refresh(format)).unwrap();

        for index in 0..10 {
            encoder.encode(&testing::frame(format, index)).unwrap();
        }
        let recovery = encoder.recover(OutputIndex(6));
        let recovered = encoder
            .encode(&testing::frame(format, 10))
            .unwrap()
            .unwrap();

        assert_eq!(recovery, Recovery::Reference(OutputIndex(6)));
        assert!(!recovered.keyframe);
        assert!(!contains_idr(&recovered.data));
    }

    #[cfg(feature = "vaapi")]
    #[test]
    fn a_stream_with_dropped_packets_decodes_after_recovery_without_an_idr() {
        use crate::{
            loss::{Handling, Loss, encode_with_loss},
            vaapi::tests::decode_vaapi,
        };

        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let resolution = Resolution::new(400, 216);
        let video = VideoConfig {
            resolution: Some(resolution),
            bitrate_kbps: 20_000,
            ..without_intra_refresh(format)
        };

        for last_good in [14, 12] {
            let loss = Loss {
                last_good,
                resumed: 20,
                count: 40,
            };
            let mut encoder = crate::open(&video).unwrap();

            let run = encode_with_loss(
                encoder.as_mut(),
                format,
                resolution,
                loss,
                Handling::Recover { acknowledged: None },
            );
            let Some(pictures) = decode_vaapi(Codec::H264, &run.kept()) else {
                return;
            };

            assert_eq!(run.referenced(), last_good);
            assert!(run.recovery_packets().iter().all(|packet| !packet.keyframe));
            assert!(
                run.recovery_packets()
                    .iter()
                    .all(|packet| !contains_idr(&packet.data))
            );
            run.check_decoded(&pictures, format, resolution);
        }
    }

    #[cfg(feature = "vaapi")]
    #[test]
    fn the_same_loss_without_recovery_does_not_decode() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        use crate::{
            loss::{Handling, Loss, encode_with_loss},
            vaapi::tests::decode_vaapi,
        };

        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let resolution = Resolution::new(400, 216);
        let video = VideoConfig {
            resolution: Some(resolution),
            bitrate_kbps: 20_000,
            ..without_intra_refresh(format)
        };
        let loss = Loss {
            last_good: 14,
            resumed: 20,
            count: 40,
        };
        let mut encoder = crate::open(&video).unwrap();

        let run = encode_with_loss(encoder.as_mut(), format, resolution, loss, Handling::Ignore);
        let broken = catch_unwind(AssertUnwindSafe(|| {
            let Some(pictures) = decode_vaapi(Codec::H264, &run.kept()) else {
                return;
            };
            run.check_decoded(&pictures, format, resolution);
        }));

        assert!(broken.is_err());
    }

    #[test]
    fn the_window_forgets_from_the_first_lost_output() {
        let mut window = ReferenceWindow::new(4);
        for pts in 0..6 {
            window.record(10 + pts, pts == 0);
        }

        assert_eq!(window.plan(OutputIndex(5)), Some(Forget::Nothing));
        assert_eq!(window.plan(OutputIndex(4)), Some(Forget::From(15)));
        assert_eq!(window.plan(OutputIndex(2)), Some(Forget::From(13)));
        assert_eq!(window.plan(OutputIndex(1)), None);
        assert_eq!(window.plan(OutputIndex(6)), None);
    }

    #[test]
    fn the_window_starts_at_the_last_keyframe() {
        let mut window = ReferenceWindow::new(8);
        for pts in 0..6 {
            window.record(pts, pts == 0 || pts == 3);
        }

        assert_eq!(window.plan(OutputIndex(3)), Some(Forget::From(4)));
        assert_eq!(window.plan(OutputIndex(2)), None);
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
    fn hdr_metadata_maps_onto_the_sei_fields() {
        let config = VideoConfig {
            color: hdr10_color(),
            ..config(PixelFormat::default(), X264Config::default())
        };
        let VideoEncoderConfig::X264(x264) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, x264, ChromaSiting::default()).unwrap();

        let param = settings.param(testing::TEST_RESOLUTION).unwrap();
        let display = &param.mastering_display;
        let light = &param.content_light_level;

        assert_eq!(display.b_mastering_display, 1);
        assert_eq!((display.i_green_x, display.i_green_y), (13_250, 34_500));
        assert_eq!((display.i_blue_x, display.i_blue_y), (7_500, 3_000));
        assert_eq!((display.i_red_x, display.i_red_y), (34_000, 16_000));
        assert_eq!((display.i_white_x, display.i_white_y), (15_635, 16_450));
        assert_eq!(
            (display.i_display_max, display.i_display_min),
            (10_000_000, 50)
        );
        assert_eq!(
            (light.b_cll, light.i_max_cll, light.i_max_fall),
            (1, 1000, 400)
        );
    }

    #[test]
    fn the_siting_reaches_the_vui_chroma_location() {
        let config = config(PixelFormat::default(), X264Config::default());
        let VideoEncoderConfig::X264(x264) = &config.encoder else {
            return;
        };

        for (siting, location) in [(ChromaSiting::Left, 0), (ChromaSiting::Center, 1)] {
            let settings = Settings::new(&config, x264, siting).unwrap();

            let param = settings.param(testing::TEST_RESOLUTION).unwrap();

            assert_eq!(param.vui.i_chroma_loc, location);
        }
    }

    #[test]
    fn sdr_streams_write_no_hdr_sei() {
        let config = config(PixelFormat::default(), X264Config::default());
        let VideoEncoderConfig::X264(x264) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, x264, ChromaSiting::default()).unwrap();

        let param = settings.param(testing::TEST_RESOLUTION).unwrap();

        assert_eq!(param.mastering_display.b_mastering_display, 0);
        assert_eq!(param.content_light_level.b_cll, 0);
    }

    #[test]
    fn colour_description_reaches_the_decoder() {
        let Ok(mut decoder) =
            rusplay_decode::VaapiDecoder::new(Codec::H264, &VaapiDecoderConfig::default())
        else {
            return;
        };

        testing::check_colour_reaches_the_decoder(
            &config(
                pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
                X264Config::default(),
            ),
            &mut decoder,
        );
    }

    #[test]
    fn passthrough_parameters_reach_the_library() {
        let config = with_params(&[("aq-mode", "3"), ("bframes", "2")]);
        let VideoEncoderConfig::X264(x264) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, x264, ChromaSiting::default()).unwrap();

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
        let settings = Settings::new(&config, x264, ChromaSiting::default()).unwrap();

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
