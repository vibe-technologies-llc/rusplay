use std::{
    collections::VecDeque,
    ffi::c_void,
    mem,
    ptr::{self, NonNull},
    slice,
};

use rusplay_capture::Frame;
use rusplay_config::{SvtAv1Config, VideoConfig};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorPrimaries, ColorRange, ColorSpace, EncodedFrame,
    FrameRate, MatrixCoefficients, PixelFormat, Resolution, Timestamp, TransferFunction,
};

use crate::{
    Backend, EncodeError, VideoEncoder,
    input::{YuvFrame, bytes_per_sample},
    software::{CParam, VBV_BUFFER_FRAMES},
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
    include!(concat!(env!("OUT_DIR"), "/svt_av1.rs"));
}

mod log;

const BACKEND: Backend = Backend::SvtAv1;
const VALIDATION_RESOLUTION: Resolution = Resolution::new(64, 64);
const MIN_BUFFER_MS: u64 = 40;
const MIN_BUFFER_LEVEL_MS: u64 = 20;

const LOW_LATENCY_DEFAULTS: [(&str, &str); 4] = [
    ("lookahead", "0"),
    ("scd", "0"),
    ("irefresh-type", "2"),
    ("keyint", "-1"),
];

const fn primaries_id(primaries: ColorPrimaries) -> Result<u32, EncodeError> {
    match primaries {
        ColorPrimaries::Bt709 => Ok(sys::EbColorPrimaries_EB_CICP_CP_BT_709),
        ColorPrimaries::DisplayP3 => Ok(sys::EbColorPrimaries_EB_CICP_CP_SMPTE_432),
        ColorPrimaries::DciP3 => Ok(sys::EbColorPrimaries_EB_CICP_CP_SMPTE_431),
        ColorPrimaries::Bt2020 => Ok(sys::EbColorPrimaries_EB_CICP_CP_BT_2020),
        ColorPrimaries::Bt601_525 => Ok(sys::EbColorPrimaries_EB_CICP_CP_BT_601),
        ColorPrimaries::Bt601_625 => Ok(sys::EbColorPrimaries_EB_CICP_CP_BT_470_B_G),
        ColorPrimaries::Custom(_) => Err(EncodeError::ColorPrimariesUnsignalable {
            backend: BACKEND,
            primaries,
        }),
    }
}

const fn matrix_id(matrix: MatrixCoefficients) -> u32 {
    match matrix {
        MatrixCoefficients::Bt709 => sys::EbMatrixCoefficients_EB_CICP_MC_BT_709,
        MatrixCoefficients::Bt601 => sys::EbMatrixCoefficients_EB_CICP_MC_BT_601,
        MatrixCoefficients::Bt2020Ncl => sys::EbMatrixCoefficients_EB_CICP_MC_BT_2020_NCL,
    }
}

const fn transfer_id(transfer: TransferFunction) -> u32 {
    match transfer {
        TransferFunction::Bt709 => sys::EbTransferCharacteristics_EB_CICP_TC_BT_709,
        TransferFunction::Srgb => sys::EbTransferCharacteristics_EB_CICP_TC_SRGB,
        TransferFunction::Gamma22 => sys::EbTransferCharacteristics_EB_CICP_TC_BT_470_M,
        TransferFunction::Pq => sys::EbTransferCharacteristics_EB_CICP_TC_SMPTE_2084,
        TransferFunction::Hlg => sys::EbTransferCharacteristics_EB_CICP_TC_HLG,
        TransferFunction::Linear => sys::EbTransferCharacteristics_EB_CICP_TC_LINEAR,
    }
}

const fn range_id(range: ColorRange) -> u32 {
    match range {
        ColorRange::Limited => 0,
        ColorRange::Full => 1,
    }
}

fn bits_per_second(kbps: u32) -> Result<u32, EncodeError> {
    kbps.checked_mul(1000)
        .filter(|&bps| bps > 0)
        .ok_or(EncodeError::BitrateRejected {
            backend: BACKEND,
            kbps,
        })
}

fn buffer_levels_ms(frame_rate: FrameRate) -> (i64, i64) {
    let numerator = u64::from(frame_rate.numerator).max(1);
    let frames_ms = 1000 * u64::from(frame_rate.denominator) * VBV_BUFFER_FRAMES / numerator;

    let maximum = frames_ms.max(MIN_BUFFER_MS);
    let level = (maximum / 2).max(MIN_BUFFER_LEVEL_MS);

    (
        i64::try_from(maximum).unwrap_or(i64::MAX),
        i64::try_from(level).unwrap_or(i64::MAX),
    )
}

fn parse(
    config: &mut sys::EbSvtAv1EncConfiguration,
    key: &str,
    value: &str,
) -> Result<(), EncodeError> {
    let pair = CParam::new(BACKEND, key, value)?;

    let status =
        unsafe { sys::svt_av1_enc_parse_parameter(config, pair.key.as_ptr(), pair.value.as_ptr()) };

    if status == sys::EbErrorType_EB_ErrorNone {
        Ok(())
    } else {
        Err(EncodeError::InvalidParameterValue {
            backend: BACKEND,
            key: key.to_owned(),
            value: value.to_owned(),
        })
    }
}

struct Handle {
    raw: NonNull<sys::EbComponentType>,
    initialised: bool,
}

unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.initialised {
            self.finish();
            unsafe { sys::svt_av1_enc_deinit(self.raw.as_ptr()) };
        }
        unsafe { sys::svt_av1_enc_deinit_handle(self.raw.as_ptr()) };
    }
}

impl Handle {
    fn finish(&self) {
        let mut header = sys::EbBufferHeaderType {
            size: u32::try_from(mem::size_of::<sys::EbBufferHeaderType>()).unwrap_or(0),
            flags: u32::try_from(sys::EB_BUFFERFLAG_EOS).unwrap_or(0),
            ..sys::EbBufferHeaderType::default()
        };

        let status = unsafe { sys::svt_av1_enc_send_picture(self.raw.as_ptr(), &raw mut header) };
        if status != sys::EbErrorType_EB_ErrorNone {
            return;
        }

        loop {
            let mut packet: *mut sys::EbBufferHeaderType = ptr::null_mut();
            let status =
                unsafe { sys::svt_av1_enc_get_packet(self.raw.as_ptr(), &raw mut packet, 1) };

            let Some(header) =
                NonNull::new(packet).filter(|_| status == sys::EbErrorType_EB_ErrorNone)
            else {
                return;
            };

            let last = unsafe { header.as_ref() }.flags
                & u32::try_from(sys::EB_BUFFERFLAG_EOS).unwrap_or(0)
                != 0;
            unsafe { sys::svt_av1_enc_release_out_buffer(&raw mut packet) };

            if last {
                return;
            }
        }
    }

    fn create(
        resolution: Resolution,
    ) -> Result<(Self, sys::EbSvtAv1EncConfiguration), EncodeError> {
        let mut config = sys::EbSvtAv1EncConfiguration::default();
        let mut raw: *mut sys::EbComponentType = ptr::null_mut();

        let status = unsafe { sys::svt_av1_enc_init_handle(&raw mut raw, &raw mut config) };

        match NonNull::new(raw) {
            Some(raw) if status == sys::EbErrorType_EB_ErrorNone => Ok((
                Self {
                    raw,
                    initialised: false,
                },
                config,
            )),
            _ => Err(EncodeError::OpenFailed {
                backend: BACKEND,
                resolution,
            }),
        }
    }

    fn start(
        &mut self,
        config: &mut sys::EbSvtAv1EncConfiguration,
        resolution: Resolution,
    ) -> Result<(), EncodeError> {
        let failed = EncodeError::OpenFailed {
            backend: BACKEND,
            resolution,
        };

        let status = unsafe { sys::svt_av1_enc_set_parameter(self.raw.as_ptr(), config) };
        if status != sys::EbErrorType_EB_ErrorNone {
            return Err(failed);
        }

        let status = unsafe { sys::svt_av1_enc_init(self.raw.as_ptr()) };
        if status != sys::EbErrorType_EB_ErrorNone {
            return Err(failed);
        }

        self.initialised = true;

        Ok(())
    }
}

struct Settings {
    config: SvtAv1Config,
    pixel_format: PixelFormat,
    color: ColorSpace,
    frame_rate: FrameRate,
    bitrate_kbps: u32,
}

impl Settings {
    fn new(video: &VideoConfig, config: &SvtAv1Config) -> Result<Self, EncodeError> {
        let format = video.pixel_format;

        if format.chroma != ChromaSubsampling::Yuv420 {
            return Err(EncodeError::ChromaUnsupported {
                backend: BACKEND,
                chroma: format.chroma,
            });
        }
        if format.depth == BitDepth::Twelve {
            return Err(EncodeError::BitDepthUnsupported {
                backend: BACKEND,
                depth: format.depth,
            });
        }

        let settings = Self {
            config: config.clone(),
            pixel_format: format,
            color: video.color,
            frame_rate: video.frame_rate,
            bitrate_kbps: video.bitrate_kbps,
        };

        let (_handle, mut library_config) = Handle::create(VALIDATION_RESOLUTION)?;
        settings.apply(&mut library_config, VALIDATION_RESOLUTION)?;

        Ok(settings)
    }

    fn apply(
        &self,
        config: &mut sys::EbSvtAv1EncConfiguration,
        resolution: Resolution,
    ) -> Result<(), EncodeError> {
        config.enc_mode = i8::try_from(self.config.preset).unwrap_or(i8::MAX);
        config.rate_control_mode = if self.config.low_latency {
            sys::SvtAv1RcMode_SVT_AV1_RC_MODE_CBR
        } else {
            sys::SvtAv1RcMode_SVT_AV1_RC_MODE_VBR
        }
        .try_into()
        .unwrap_or(u8::MAX);

        if self.config.low_latency {
            config.pred_structure = sys::PredStructure_LOW_DELAY;
            config.rtc = true;
            let (maximum, level) = buffer_levels_ms(self.frame_rate);
            config.maximum_buffer_size_ms = maximum;
            config.optimal_buffer_level_ms = level;
            config.starting_buffer_level_ms = level;

            for (key, value) in LOW_LATENCY_DEFAULTS {
                parse(config, key, value)?;
            }
        }
        config.force_key_frames = !self.config.low_latency;

        for (key, value) in &self.config.params {
            parse(config, key, value)?;
        }

        config.source_width = resolution.width;
        config.source_height = resolution.height;
        config.frame_rate_numerator = self.frame_rate.numerator;
        config.frame_rate_denominator = self.frame_rate.denominator;
        config.encoder_bit_depth = u32::from(self.pixel_format.depth.bits());
        config.encoder_color_format = sys::EbColorFormat_EB_YUV420;
        config.target_bit_rate = bits_per_second(self.bitrate_kbps)?;
        config.color_primaries = primaries_id(self.color.primaries)?;
        config.matrix_coefficients = matrix_id(self.color.matrix_coefficients());
        config.transfer_characteristics = transfer_id(self.color.transfer);
        config.color_range = range_id(self.color.range);

        if let Some(threads) = self.config.threads {
            config.level_of_parallelism = threads.get();
        }

        Ok(())
    }
}

struct Session {
    handle: Handle,
    resolution: Resolution,
    bytes_per_sample: usize,
    blocking_output: bool,
    ready: VecDeque<EncodedFrame>,
    sequence_qp: u32,
    pending_bitrate_kbps: Option<u32>,
}

impl Session {
    fn open(settings: &Settings, resolution: Resolution) -> Result<Self, EncodeError> {
        let (mut handle, mut config) = Handle::create(resolution)?;
        settings.apply(&mut config, resolution)?;
        handle.start(&mut config, resolution)?;

        Ok(Self {
            handle,
            resolution,
            bytes_per_sample: bytes_per_sample(settings.pixel_format.depth),
            blocking_output: config.pred_structure == sys::PredStructure_LOW_DELAY,
            ready: VecDeque::new(),
            sequence_qp: config.qp,
            pending_bitrate_kbps: None,
        })
    }

    fn encode(
        &mut self,
        view: &YuvFrame<'_>,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, EncodeError> {
        let strides = view
            .planes
            .map(|plane| u32::try_from(plane.stride / self.bytes_per_sample).unwrap_or(u32::MAX));
        let total: usize = view.planes.iter().map(|plane| plane.data.len()).sum();

        let mut samples = sys::EbSvtIOFormat {
            luma: view.planes[0].data.as_ptr().cast_mut(),
            cb: view.planes[1].data.as_ptr().cast_mut(),
            cr: view.planes[2].data.as_ptr().cast_mut(),
            y_stride: strides[0],
            cb_stride: strides[1],
            cr_stride: strides[2],
        };

        let mut rate = sys::SvtAv1RateInfo {
            seq_qp: self.sequence_qp,
            target_bit_rate: 0,
        };
        let mut rate_node = sys::EbPrivDataNode::default();
        let mut header = sys::EbBufferHeaderType::default();

        if let Some(kbps) = self.pending_bitrate_kbps {
            rate.target_bit_rate = bits_per_second(kbps)?;
            rate_node.node_type = sys::PrivDataType_RATE_CHANGE_EVENT;
            rate_node.data = (&raw mut rate).cast::<c_void>();
            rate_node.size = u32::try_from(mem::size_of::<sys::SvtAv1RateInfo>()).unwrap_or(0);
            header.p_app_private = (&raw mut rate_node).cast::<c_void>();
        }

        header.size = u32::try_from(mem::size_of::<sys::EbBufferHeaderType>()).unwrap_or(0);
        header.p_buffer = (&raw mut samples).cast::<u8>();
        header.n_filled_len = u32::try_from(total).unwrap_or(u32::MAX);
        header.n_alloc_len = header.n_filled_len;
        header.pts = i64::try_from(timestamp.as_micros()).unwrap_or(i64::MAX);
        header.pic_type = if keyframe {
            sys::EbAv1PictureType_EB_AV1_KEY_PICTURE
        } else {
            sys::EbAv1PictureType_EB_AV1_INVALID_PICTURE
        };

        let status =
            unsafe { sys::svt_av1_enc_send_picture(self.handle.raw.as_ptr(), &raw mut header) };
        if status != sys::EbErrorType_EB_ErrorNone {
            return Err(EncodeError::EncodeFailed {
                backend: BACKEND,
                code: status,
            });
        }
        self.pending_bitrate_kbps = None;

        self.receive()
    }

    fn receive(&mut self) -> Result<Option<EncodedFrame>, EncodeError> {
        while let Some(frame) = self.take_packet()? {
            self.ready.push_back(frame);

            if self.blocking_output {
                break;
            }
        }

        Ok(self.ready.pop_front())
    }

    fn take_packet(&mut self) -> Result<Option<EncodedFrame>, EncodeError> {
        let mut packet: *mut sys::EbBufferHeaderType = ptr::null_mut();

        let status =
            unsafe { sys::svt_av1_enc_get_packet(self.handle.raw.as_ptr(), &raw mut packet, 0) };

        if status == sys::EbErrorType_EB_NoErrorEmptyQueue {
            return Ok(None);
        }

        let Some(header) = NonNull::new(packet).filter(|_| status == sys::EbErrorType_EB_ErrorNone)
        else {
            return Err(EncodeError::EncodeFailed {
                backend: BACKEND,
                code: status,
            });
        };

        let (data, pts, picture_type) = {
            let header = unsafe { header.as_ref() };
            let length = header.n_filled_len as usize;
            let data = if header.p_buffer.is_null() {
                Vec::new()
            } else {
                unsafe { slice::from_raw_parts(header.p_buffer, length) }.to_vec()
            };

            (data, header.pts, header.pic_type)
        };

        unsafe { sys::svt_av1_enc_release_out_buffer(&raw mut packet) };

        Ok(Some(EncodedFrame {
            codec: Codec::Av1,
            timestamp: Timestamp::from_micros(u64::try_from(pts).unwrap_or(0)),
            keyframe: picture_type == sys::EbAv1PictureType_EB_AV1_KEY_PICTURE,
            data,
        }))
    }
}

pub struct SvtAv1Encoder {
    settings: Settings,
    session: Option<Session>,
    keyframe_pending: bool,
}

impl SvtAv1Encoder {
    pub fn new(video: &VideoConfig, config: &SvtAv1Config) -> Result<Self, EncodeError> {
        log::install(config.log_level);
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

impl VideoEncoder for SvtAv1Encoder {
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
        bits_per_second(kbps)?;

        self.settings.bitrate_kbps = kbps;
        if let Some(session) = &mut self.session {
            session.pending_bitrate_kbps = Some(kbps);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rusplay_config::{SvtAv1LogLevel, VideoEncoderConfig};

    use super::*;
    use crate::testing::{self, pixel, video};

    const SEQUENCE_HEADER_OBU: u8 = 1;
    const OBU_HAS_SIZE_FLAG: u8 = 0b10;
    const OBU_EXTENSION_FLAG: u8 = 0b100;

    fn obu_types(data: &[u8]) -> Vec<u8> {
        let mut types = Vec::new();
        let mut cursor = 0;

        while let Some(&header) = data.get(cursor) {
            types.push((header >> 3) & 0xf);
            if header & OBU_HAS_SIZE_FLAG == 0 {
                break;
            }
            cursor += 1 + usize::from(header & OBU_EXTENSION_FLAG != 0);

            let mut size = 0_usize;
            let mut shift = 0;
            while let Some(&byte) = data.get(cursor) {
                cursor += 1;
                size |= usize::from(byte & 0x7f) << shift;
                shift += 7;
                if byte & 0x80 == 0 {
                    break;
                }
            }
            cursor += size;
        }

        types
    }

    fn contains_sequence_header(data: &[u8]) -> bool {
        obu_types(data).contains(&SEQUENCE_HEADER_OBU)
    }

    fn config(format: PixelFormat, svt: SvtAv1Config) -> VideoConfig {
        video(Codec::Av1, format, VideoEncoderConfig::SvtAv1(svt))
    }

    fn with_params(pairs: &[(&str, &str)]) -> VideoConfig {
        config(
            pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            SvtAv1Config {
                params: testing::params(pairs),
                ..SvtAv1Config::default()
            },
        )
    }

    #[test]
    fn every_log_level_opens_and_encodes() {
        for log_level in [
            SvtAv1LogLevel::Off,
            SvtAv1LogLevel::Fatal,
            SvtAv1LogLevel::Error,
            SvtAv1LogLevel::Warn,
            SvtAv1LogLevel::Info,
            SvtAv1LogLevel::Debug,
        ] {
            testing::check_stream_starts_on_a_keyframe(
                &config(
                    pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
                    SvtAv1Config {
                        log_level,
                        ..SvtAv1Config::default()
                    },
                ),
                contains_sequence_header,
            );
        }
    }

    #[test]
    fn stream_starts_on_a_keyframe_for_both_depths() {
        for depth in [BitDepth::Eight, BitDepth::Ten] {
            testing::check_stream_starts_on_a_keyframe(
                &config(
                    pixel(ChromaSubsampling::Yuv420, depth),
                    SvtAv1Config::default(),
                ),
                contains_sequence_header,
            );
        }
    }

    #[test]
    fn requested_keyframe_is_a_keyframe() {
        testing::check_keyframe_on_request(
            &config(
                pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
                SvtAv1Config::default(),
            ),
            contains_sequence_header,
        );
    }

    #[test]
    fn set_bitrate_applies_to_the_next_frame() {
        testing::check_set_bitrate_in_place(&config(
            pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            SvtAv1Config::default(),
        ));
    }

    #[test]
    fn rejected_parameter_names_the_key() {
        testing::check_invalid_value_names_the_key(
            &with_params(&[("no-such-knob", "1")]),
            "no-such-knob",
            "1",
        );
    }

    #[test]
    fn invalid_parameter_value_names_the_key() {
        testing::check_invalid_value_names_the_key(
            &with_params(&[("lookahead", "plenty")]),
            "lookahead",
            "plenty",
        );
    }

    #[test]
    fn passthrough_parameters_reach_the_library() {
        let config = with_params(&[("film-grain", "7"), ("tile-rows", "1")]);
        let VideoEncoderConfig::SvtAv1(svt) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, svt).unwrap();
        let (_handle, mut library) = Handle::create(testing::TEST_RESOLUTION).unwrap();

        settings
            .apply(&mut library, testing::TEST_RESOLUTION)
            .unwrap();

        assert_eq!(library.film_grain_denoise_strength, 7);
        assert_eq!(library.tile_rows, 1);
    }

    #[test]
    fn typed_fields_win_over_params() {
        let config = with_params(&[("tbr", "1"), ("fps-num", "1")]);
        let VideoEncoderConfig::SvtAv1(svt) = &config.encoder else {
            return;
        };
        let settings = Settings::new(&config, svt).unwrap();
        let (_handle, mut library) = Handle::create(testing::TEST_RESOLUTION).unwrap();

        settings
            .apply(&mut library, testing::TEST_RESOLUTION)
            .unwrap();

        assert_eq!(library.target_bit_rate, 8_000_000);
        assert_eq!(library.frame_rate_numerator, 60);
    }

    #[test]
    fn other_chroma_formats_are_unsupported() {
        for chroma in [ChromaSubsampling::Yuv422, ChromaSubsampling::Yuv444] {
            let error = crate::open(&config(
                pixel(chroma, BitDepth::Eight),
                SvtAv1Config::default(),
            ))
            .err()
            .unwrap();

            assert!(matches!(
                error,
                EncodeError::ChromaUnsupported { chroma: got, .. } if got == chroma
            ));
        }
    }

    #[test]
    fn twelve_bit_is_unsupported() {
        let error = crate::open(&config(
            pixel(ChromaSubsampling::Yuv420, BitDepth::Twelve),
            SvtAv1Config::default(),
        ))
        .err()
        .unwrap();

        assert!(matches!(error, EncodeError::BitDepthUnsupported { .. }));
    }

    #[test]
    fn wrong_frame_inputs_are_rejected() {
        testing::check_wrong_inputs_are_rejected(&config(
            pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            SvtAv1Config::default(),
        ));
    }

    #[test]
    fn unspecified_resolution_follows_the_first_frame() {
        testing::check_unspecified_resolution_follows_the_first_frame(&config(
            pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            SvtAv1Config::default(),
        ));
    }
}
