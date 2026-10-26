use std::ptr;

use rusplay_config::{NvencPreset, NvencTuning, RateControl};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorRange, ColorSpace, FrameRate, HdrLevels, PixelFormat,
    Resolution,
};

use crate::{
    hdr::{St2086, content_light_payload},
    nvenc::{
        ffi::{self, Guid, Plain, av1_bits, h264_bits, hevc_bits, initialize_bits, rc_bits},
        layout::buffer_format,
        params::NvencParams,
    },
    rate::vbv_buffer_kbit,
};

const SEI_MASTERING_DISPLAY: u32 = 137;
const SEI_CONTENT_LIGHT: u32 = 144;
const AV1_METADATA_CONTENT_LIGHT: u32 = 1;
const AV1_METADATA_MASTERING_DISPLAY: u32 = 2;
const AV1_QINDEX_PER_QP: u32 = 5;
const AV1_MAX_QINDEX: u32 = 255;

#[derive(Debug, Clone)]
pub(super) struct Settings {
    pub codec: Codec,
    pub format: PixelFormat,
    pub color: ColorSpace,
    pub frame_rate: FrameRate,
    pub resolution: Option<Resolution>,
    pub bitrate_kbps: u32,
    pub rate_control: RateControl,
    pub preset: NvencPreset,
    pub tuning: NvencTuning,
    pub params: NvencParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Features {
    pub intra_refresh: bool,
    pub temporal_aq: bool,
}

pub(super) const fn codec_guid(codec: Codec) -> Guid {
    match codec {
        Codec::H264 => ffi::CODEC_H264_GUID,
        Codec::Hevc => ffi::CODEC_HEVC_GUID,
        Codec::Av1 => ffi::CODEC_AV1_GUID,
    }
}

pub(super) const fn profile(codec: Codec, format: PixelFormat) -> Option<Guid> {
    match (codec, format.chroma, format.depth) {
        (Codec::H264, ChromaSubsampling::Yuv420, BitDepth::Eight) => {
            Some(ffi::H264_PROFILE_HIGH_GUID)
        }
        (Codec::H264, ChromaSubsampling::Yuv444, BitDepth::Eight) => {
            Some(ffi::H264_PROFILE_HIGH_444_GUID)
        }
        (Codec::Hevc, ChromaSubsampling::Yuv420, BitDepth::Eight) => {
            Some(ffi::HEVC_PROFILE_MAIN_GUID)
        }
        (Codec::Hevc, ChromaSubsampling::Yuv420, BitDepth::Ten) => {
            Some(ffi::HEVC_PROFILE_MAIN10_GUID)
        }
        (Codec::Hevc, ChromaSubsampling::Yuv444, BitDepth::Eight | BitDepth::Ten) => {
            Some(ffi::HEVC_PROFILE_FREXT_GUID)
        }
        (Codec::Av1, ChromaSubsampling::Yuv420, BitDepth::Eight | BitDepth::Ten) => {
            Some(ffi::AV1_PROFILE_MAIN_GUID)
        }
        _ => None,
    }
}

pub(super) const fn preset_guid(preset: NvencPreset) -> Guid {
    match preset {
        NvencPreset::P1 => ffi::PRESET_P1_GUID,
        NvencPreset::P2 => ffi::PRESET_P2_GUID,
        NvencPreset::P3 => ffi::PRESET_P3_GUID,
        NvencPreset::P4 => ffi::PRESET_P4_GUID,
        NvencPreset::P5 => ffi::PRESET_P5_GUID,
        NvencPreset::P6 => ffi::PRESET_P6_GUID,
        NvencPreset::P7 => ffi::PRESET_P7_GUID,
    }
}

pub(super) const fn tuning_info(tuning: NvencTuning) -> u32 {
    match tuning {
        NvencTuning::UltraLowLatency => ffi::TUNING_ULTRA_LOW_LATENCY,
        NvencTuning::LowLatency => ffi::TUNING_LOW_LATENCY,
        NvencTuning::HighQuality => ffi::TUNING_HIGH_QUALITY,
    }
}

pub(super) const fn chroma_format_idc(format: PixelFormat) -> u32 {
    match format.chroma {
        ChromaSubsampling::Yuv444 => ffi::CHROMA_FORMAT_IDC_444,
        ChromaSubsampling::Yuv420 | ChromaSubsampling::Yuv422 => ffi::CHROMA_FORMAT_IDC_420,
    }
}

pub(super) const fn bit_depth(format: PixelFormat) -> u32 {
    match format.depth {
        BitDepth::Eight => ffi::BIT_DEPTH_8,
        BitDepth::Ten | BitDepth::Twelve => ffi::BIT_DEPTH_10,
    }
}

pub(super) const fn rate_control_supported(modes: i32, rate_control: RateControl) -> bool {
    let mode = rate_control_mode(rate_control);
    mode == ffi::RC_CONSTQP || modes.cast_unsigned() & mode != 0
}

pub(super) const fn rate_control_mode(rate_control: RateControl) -> u32 {
    match rate_control {
        RateControl::Cbr => ffi::RC_CBR,
        RateControl::Vbr { .. } => ffi::RC_VBR,
        RateControl::ConstantQuality { .. } => ffi::RC_CONSTQP,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RateValues {
    pub mode: u32,
    pub average_bits: u32,
    pub max_bits: u32,
    pub vbv_bits: u32,
    pub qp: Option<u32>,
}

impl Settings {
    fn peak_kbps(&self) -> u32 {
        match self.rate_control {
            RateControl::Vbr { max_bitrate_kbps } => max_bitrate_kbps.max(self.bitrate_kbps),
            RateControl::Cbr | RateControl::ConstantQuality { .. } => self.bitrate_kbps,
        }
    }

    pub(super) fn rate_values(&self) -> RateValues {
        let mode = rate_control_mode(self.rate_control);
        if let RateControl::ConstantQuality { qp } = self.rate_control {
            let qp = match self.codec {
                Codec::Av1 => (u32::from(qp) * AV1_QINDEX_PER_QP).min(AV1_MAX_QINDEX),
                Codec::H264 | Codec::Hevc => u32::from(qp),
            };
            return RateValues {
                mode,
                average_bits: 0,
                max_bits: 0,
                vbv_bits: 0,
                qp: Some(qp),
            };
        }

        let peak = self.peak_kbps();
        RateValues {
            mode,
            average_bits: self.bitrate_kbps.saturating_mul(1000),
            max_bits: peak.saturating_mul(1000),
            vbv_bits: vbv_buffer_kbit(peak, self.frame_rate).saturating_mul(1000),
            qp: None,
        }
    }

    pub(super) fn intra_refresh(&self) -> Option<(u32, u32)> {
        let frames = self
            .params
            .intra_refresh_frames
            .unwrap_or_else(|| default_refresh_frames(self.frame_rate));

        (frames > 0).then(|| (frames.saturating_add(1), frames))
    }
}

fn default_refresh_frames(frame_rate: FrameRate) -> u32 {
    frame_rate
        .numerator
        .div_ceil(frame_rate.denominator.max(1))
        .max(1)
}

pub(super) fn apply_rate(rc: &mut ffi::RcParams, values: RateValues) {
    rc.version = ffi::RC_PARAMS_VER;
    rc.rate_control_mode = values.mode;
    rc.average_bit_rate = values.average_bits;
    rc.max_bit_rate = values.max_bits;
    rc.vbv_buffer_size = values.vbv_bits;
    rc.vbv_initial_delay = values.vbv_bits;
    if let Some(qp) = values.qp {
        rc.const_qp = ffi::Qp {
            inter_p: qp,
            inter_b: qp,
            intra: qp,
        };
    }
}

pub(super) fn vui(color: &ColorSpace) -> ffi::VuiParameters {
    ffi::VuiParameters {
        video_signal_type_present_flag: 1,
        video_format: ffi::VUI_VIDEO_FORMAT_UNSPECIFIED,
        video_full_range_flag: u32::from(color.range == ColorRange::Full),
        colour_description_present_flag: 1,
        colour_primaries: u32::from(color.primaries.h273_code().unwrap_or(2)),
        transfer_characteristics: u32::from(color.transfer.h273_code()),
        colour_matrix: u32::from(color.matrix_coefficients().h273_code()),
        ..ffi::VuiParameters::default()
    }
}

pub(super) fn configure(config: &mut ffi::Config, settings: &Settings, features: Features) {
    config.version = ffi::CONFIG_VER;
    if let Some(profile) = profile(settings.codec, settings.format) {
        config.profile_guid = profile;
    }
    config.gop_length = ffi::INFINITE_GOP_LENGTH;
    config.frame_interval_p = 1;

    let rc = &mut config.rc_params;
    apply_rate(rc, settings.rate_values());
    rc.lookahead_depth = 0;
    rc_bits::ENABLE_LOOKAHEAD.set(&mut rc.flags, 0);
    rc_bits::ZERO_REORDER_DELAY.set(&mut rc.flags, 1);
    let params = &settings.params;
    if let Some(spatial) = params.spatial_aq {
        rc_bits::ENABLE_AQ.set(&mut rc.flags, u32::from(spatial));
    }
    if let Some(strength) = params.aq_strength {
        rc_bits::AQ_STRENGTH.set(&mut rc.flags, strength);
    }
    if let Some(temporal) = params.temporal_aq {
        rc_bits::ENABLE_TEMPORAL_AQ.set(&mut rc.flags, u32::from(temporal && features.temporal_aq));
    }
    if let Some(multipass) = params.multipass {
        rc.multi_pass = multipass.nvenc();
    }

    let refresh = settings.intra_refresh().filter(|_| features.intra_refresh);
    let (refresh_on, refresh_period, refresh_count) = match refresh {
        Some((period, count)) => (1, period, count),
        None => (0, 0, 0),
    };
    let idc = chroma_format_idc(settings.format);
    let depth = bit_depth(settings.format);
    let color = &settings.color;

    match settings.codec {
        Codec::H264 => {
            let h264 = unsafe { &mut config.codec.h264 };
            h264.idr_period = ffi::INFINITE_GOP_LENGTH;
            h264_bits::REPEAT_SPSPPS.set(&mut h264.flags, 1);
            h264_bits::OUTPUT_AUD.set(&mut h264.flags, 0);
            h264_bits::ENABLE_INTRA_REFRESH.set(&mut h264.flags, refresh_on);
            h264.intra_refresh_period = refresh_period;
            h264.intra_refresh_cnt = refresh_count;
            h264.max_num_ref_frames = 1;
            h264.chroma_format_idc = idc;
            h264.separate_colour_plane_flag = 0;
            h264.output_bit_depth = depth;
            h264.input_bit_depth = depth;
            h264.vui = vui(color);
        }
        Codec::Hevc => {
            let hevc = unsafe { &mut config.codec.hevc };
            hevc.idr_period = ffi::INFINITE_GOP_LENGTH;
            hevc_bits::REPEAT_SPSPPS.set(&mut hevc.flags, 1);
            hevc_bits::OUTPUT_AUD.set(&mut hevc.flags, 0);
            hevc_bits::ENABLE_INTRA_REFRESH.set(&mut hevc.flags, refresh_on);
            hevc_bits::CHROMA_FORMAT_IDC.set(&mut hevc.flags, idc);
            hevc.intra_refresh_period = refresh_period;
            hevc.intra_refresh_cnt = refresh_count;
            hevc.max_num_ref_frames_in_dpb = 1;
            hevc.output_bit_depth = depth;
            hevc.input_bit_depth = depth;
            hevc.vui = vui(color);
        }
        Codec::Av1 => {
            let av1 = unsafe { &mut config.codec.av1 };
            av1.idr_period = ffi::INFINITE_GOP_LENGTH;
            av1_bits::REPEAT_SEQ_HDR.set(&mut av1.flags, 1);
            av1_bits::ENABLE_INTRA_REFRESH.set(&mut av1.flags, refresh_on);
            av1_bits::CHROMA_FORMAT_IDC.set(&mut av1.flags, idc);
            av1.intra_refresh_period = refresh_period;
            av1.intra_refresh_cnt = refresh_count;
            av1.max_num_ref_frames_in_dpb = 1;
            av1.output_bit_depth = depth;
            av1.input_bit_depth = depth;
            let signalled = vui(color);
            av1.color_primaries = signalled.colour_primaries;
            av1.transfer_characteristics = signalled.transfer_characteristics;
            av1.matrix_coefficients = signalled.colour_matrix;
            av1.color_range = if color.range == ColorRange::Full {
                ffi::AV1_COLOR_RANGE_FULL
            } else {
                ffi::AV1_COLOR_RANGE_STUDIO
            };
            av1.chroma_sample_position = 0;
        }
    }
}

pub(super) fn initialize_params(
    settings: &Settings,
    resolution: Resolution,
    config: &mut ffi::Config,
) -> ffi::InitializeParams {
    let mut params = ffi::InitializeParams::zeroed();
    params.version = ffi::INITIALIZE_PARAMS_VER;
    params.encode_guid = codec_guid(settings.codec);
    params.preset_guid = preset_guid(settings.preset);
    params.encode_width = resolution.width;
    params.encode_height = resolution.height;
    params.dar_width = resolution.width;
    params.dar_height = resolution.height;
    params.frame_rate_num = settings.frame_rate.numerator;
    params.frame_rate_den = settings.frame_rate.denominator.max(1);
    params.enable_encode_async = 0;
    params.enable_ptd = 1;
    params.encode_config = ptr::from_mut(config);
    params.max_encode_width = resolution.width;
    params.max_encode_height = resolution.height;
    params.tuning_info = tuning_info(settings.tuning);
    params.buffer_format = buffer_format(settings.format).unwrap_or(0);
    if let Some(split) = settings.params.split_encode {
        initialize_bits::SPLIT_ENCODE_MODE.set(&mut params.flags, split.nvenc());
    }

    params
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Payload {
    pub kind: u32,
    pub bytes: Vec<u8>,
}

pub(super) fn hdr_payloads(codec: Codec, hdr: &HdrLevels) -> Vec<Payload> {
    let (mastering_kind, light_kind) = match codec {
        Codec::H264 | Codec::Hevc => (SEI_MASTERING_DISPLAY, SEI_CONTENT_LIGHT),
        Codec::Av1 => (AV1_METADATA_MASTERING_DISPLAY, AV1_METADATA_CONTENT_LIGHT),
    };

    let mastering = hdr.mastering.map(|display| {
        let units = St2086::of(&display);
        Payload {
            kind: mastering_kind,
            bytes: match codec {
                Codec::H264 | Codec::Hevc => units.sei_payload().to_vec(),
                Codec::Av1 => units.av1_payload().to_vec(),
            },
        }
    });
    let light = hdr.content_light.map(|light| Payload {
        kind: light_kind,
        bytes: content_light_payload(light).to_vec(),
    });

    mastering.into_iter().chain(light).collect()
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, MatrixCoefficients, TransferFunction};

    use super::*;
    use crate::{hdr::fixtures::hdr10_color, nvenc::params::SplitEncode};

    fn pixel(chroma: ChromaSubsampling, depth: BitDepth) -> PixelFormat {
        PixelFormat { chroma, depth }
    }

    fn settings(codec: Codec, format: PixelFormat) -> Settings {
        Settings {
            codec,
            format,
            color: ColorSpace::default(),
            frame_rate: FrameRate::new(60, 1),
            resolution: None,
            bitrate_kbps: 20_000,
            rate_control: RateControl::Cbr,
            preset: NvencPreset::P1,
            tuning: NvencTuning::UltraLowLatency,
            params: NvencParams::default(),
        }
    }

    fn configured(settings: &Settings, features: Features) -> ffi::Config {
        let mut config = ffi::Config::zeroed();
        configure(&mut config, settings, features);
        config
    }

    const ALL_FEATURES: Features = Features {
        intra_refresh: true,
        temporal_aq: true,
    };

    #[test]
    fn profiles_follow_codec_chroma_and_depth() {
        let yuv420_8 = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let yuv420_10 = pixel(ChromaSubsampling::Yuv420, BitDepth::Ten);
        let yuv444_8 = pixel(ChromaSubsampling::Yuv444, BitDepth::Eight);
        let yuv444_10 = pixel(ChromaSubsampling::Yuv444, BitDepth::Ten);
        let yuv422_8 = pixel(ChromaSubsampling::Yuv422, BitDepth::Eight);

        assert_eq!(
            profile(Codec::H264, yuv420_8),
            Some(ffi::H264_PROFILE_HIGH_GUID)
        );
        assert_eq!(
            profile(Codec::H264, yuv444_8),
            Some(ffi::H264_PROFILE_HIGH_444_GUID)
        );
        assert_eq!(profile(Codec::H264, yuv420_10), None);
        assert_eq!(
            profile(Codec::Hevc, yuv420_8),
            Some(ffi::HEVC_PROFILE_MAIN_GUID)
        );
        assert_eq!(
            profile(Codec::Hevc, yuv420_10),
            Some(ffi::HEVC_PROFILE_MAIN10_GUID)
        );
        assert_eq!(
            profile(Codec::Hevc, yuv444_8),
            Some(ffi::HEVC_PROFILE_FREXT_GUID)
        );
        assert_eq!(
            profile(Codec::Hevc, yuv444_10),
            Some(ffi::HEVC_PROFILE_FREXT_GUID)
        );
        assert_eq!(
            profile(Codec::Av1, yuv420_10),
            Some(ffi::AV1_PROFILE_MAIN_GUID)
        );
        assert_eq!(profile(Codec::Av1, yuv444_8), None);
        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            assert_eq!(profile(codec, yuv422_8), None);
            assert_eq!(
                profile(codec, pixel(ChromaSubsampling::Yuv420, BitDepth::Twelve)),
                None
            );
        }
    }

    #[test]
    fn presets_and_tunings_map_to_the_sdk_identifiers() {
        assert_eq!(preset_guid(NvencPreset::P1), ffi::PRESET_P1_GUID);
        assert_eq!(preset_guid(NvencPreset::P4).data1, 0x90a7_b826);
        assert_eq!(preset_guid(NvencPreset::P7).data1, 0x8484_8c12);
        assert_eq!(tuning_info(NvencTuning::UltraLowLatency), 3);
        assert_eq!(tuning_info(NvencTuning::LowLatency), 2);
        assert_eq!(tuning_info(NvencTuning::HighQuality), 1);
        assert_eq!(codec_guid(Codec::Av1), ffi::CODEC_AV1_GUID);
    }

    #[test]
    fn cbr_runs_at_the_bitrate_with_a_two_frame_buffer() {
        let values = settings(Codec::H264, PixelFormat::default()).rate_values();

        assert_eq!(
            values,
            RateValues {
                mode: ffi::RC_CBR,
                average_bits: 20_000_000,
                max_bits: 20_000_000,
                vbv_bits: 666_000,
                qp: None,
            }
        );
    }

    #[test]
    fn vbr_peaks_at_the_maximum_and_sizes_the_buffer_from_it() {
        let mut vbr = settings(Codec::Hevc, PixelFormat::default());
        vbr.rate_control = RateControl::Vbr {
            max_bitrate_kbps: 60_000,
        };

        let values = vbr.rate_values();

        assert_eq!(values.mode, ffi::RC_VBR);
        assert_eq!(values.average_bits, 20_000_000);
        assert_eq!(values.max_bits, 60_000_000);
        assert_eq!(values.vbv_bits, 2_000_000);
    }

    #[test]
    fn constant_quality_sets_every_qp_and_scales_for_av1() {
        let mut cq = settings(Codec::H264, PixelFormat::default());
        cq.rate_control = RateControl::ConstantQuality { qp: 23 };
        let mut config = configured(&cq, Features::default());

        assert_eq!(config.rc_params.rate_control_mode, ffi::RC_CONSTQP);
        assert_eq!(
            config.rc_params.const_qp,
            ffi::Qp {
                inter_p: 23,
                inter_b: 23,
                intra: 23
            }
        );

        cq.codec = Codec::Av1;
        config = configured(&cq, Features::default());

        assert_eq!(config.rc_params.const_qp.intra, 115);

        cq.rate_control = RateControl::ConstantQuality { qp: 51 };

        assert_eq!(cq.rate_values().qp, Some(255));
    }

    #[test]
    fn rate_control_support_reads_the_caps_bitmask() {
        assert!(rate_control_supported(
            0,
            RateControl::DEFAULT_CONSTANT_QUALITY
        ));
        assert!(rate_control_supported(0b11, RateControl::Cbr));
        assert!(!rate_control_supported(0b01, RateControl::Cbr));
        assert!(rate_control_supported(0b01, RateControl::DEFAULT_VBR));
    }

    #[test]
    fn low_latency_structure_is_forced_over_the_preset() {
        let mut config = ffi::Config::zeroed();
        config.gop_length = 250;
        config.frame_interval_p = 3;
        config.rc_params.lookahead_depth = 16;
        rc_bits::ENABLE_LOOKAHEAD.set(&mut config.rc_params.flags, 1);

        configure(
            &mut config,
            &settings(Codec::Hevc, PixelFormat::default()),
            ALL_FEATURES,
        );

        let hevc = unsafe { config.codec.hevc };
        assert_eq!(config.version, ffi::CONFIG_VER);
        assert_eq!(config.gop_length, ffi::INFINITE_GOP_LENGTH);
        assert_eq!(config.frame_interval_p, 1);
        assert_eq!(config.rc_params.lookahead_depth, 0);
        assert_eq!(rc_bits::ENABLE_LOOKAHEAD.get(config.rc_params.flags), 0);
        assert_eq!(hevc.idr_period, ffi::INFINITE_GOP_LENGTH);
        assert_eq!(hevc_bits::REPEAT_SPSPPS.get(hevc.flags), 1);
        assert_eq!(hevc.max_num_ref_frames_in_dpb, 1);
    }

    #[test]
    fn intra_refresh_sweeps_once_per_second_by_default() {
        let config = configured(&settings(Codec::H264, PixelFormat::default()), ALL_FEATURES);
        let h264 = unsafe { config.codec.h264 };

        assert_eq!(h264_bits::ENABLE_INTRA_REFRESH.get(h264.flags), 1);
        assert_eq!(h264.intra_refresh_cnt, 60);
        assert_eq!(h264.intra_refresh_period, 61);

        let mut off = settings(Codec::H264, PixelFormat::default());
        off.params.intra_refresh_frames = Some(0);
        let config = configured(&off, ALL_FEATURES);
        let h264 = unsafe { config.codec.h264 };

        assert_eq!(h264_bits::ENABLE_INTRA_REFRESH.get(h264.flags), 0);

        let config = configured(
            &settings(Codec::H264, PixelFormat::default()),
            Features::default(),
        );
        let h264 = unsafe { config.codec.h264 };

        assert_eq!(h264_bits::ENABLE_INTRA_REFRESH.get(h264.flags), 0);
    }

    #[test]
    fn chroma_and_depth_reach_every_codec_config() {
        let yuv444_10 = pixel(ChromaSubsampling::Yuv444, BitDepth::Ten);
        let yuv420_10 = pixel(ChromaSubsampling::Yuv420, BitDepth::Ten);
        let yuv444_8 = pixel(ChromaSubsampling::Yuv444, BitDepth::Eight);

        let hevc = unsafe {
            configured(&settings(Codec::Hevc, yuv444_10), ALL_FEATURES)
                .codec
                .hevc
        };
        let av1 = unsafe {
            configured(&settings(Codec::Av1, yuv420_10), ALL_FEATURES)
                .codec
                .av1
        };
        let h264 = unsafe {
            configured(&settings(Codec::H264, yuv444_8), ALL_FEATURES)
                .codec
                .h264
        };

        assert_eq!(hevc_bits::CHROMA_FORMAT_IDC.get(hevc.flags), 3);
        assert_eq!(hevc.input_bit_depth, 10);
        assert_eq!(hevc.output_bit_depth, 10);
        assert_eq!(av1_bits::CHROMA_FORMAT_IDC.get(av1.flags), 1);
        assert_eq!(av1.output_bit_depth, 10);
        assert_eq!(h264.chroma_format_idc, 3);
        assert_eq!(h264.output_bit_depth, 8);
    }

    #[test]
    fn colour_description_lands_in_the_vui_and_av1_colour_config() {
        let mut hdr = settings(Codec::Hevc, pixel(ChromaSubsampling::Yuv420, BitDepth::Ten));
        hdr.color = hdr10_color();
        let hevc = unsafe { configured(&hdr, ALL_FEATURES).codec.hevc };

        assert_eq!(hevc.vui.video_signal_type_present_flag, 1);
        assert_eq!(hevc.vui.colour_description_present_flag, 1);
        assert_eq!(hevc.vui.colour_primaries, 9);
        assert_eq!(hevc.vui.transfer_characteristics, 16);
        assert_eq!(hevc.vui.colour_matrix, 9);
        assert_eq!(hevc.vui.video_full_range_flag, 0);

        hdr.codec = Codec::Av1;
        hdr.color.range = ColorRange::Full;
        let av1 = unsafe { configured(&hdr, ALL_FEATURES).codec.av1 };

        assert_eq!(av1.color_primaries, 9);
        assert_eq!(av1.transfer_characteristics, 16);
        assert_eq!(av1.matrix_coefficients, 9);
        assert_eq!(av1.color_range, ffi::AV1_COLOR_RANGE_FULL);

        let srgb = ColorSpace {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferFunction::Srgb,
            matrix: Some(MatrixCoefficients::Bt709),
            range: ColorRange::Full,
            ..ColorSpace::default()
        };
        let signalled = vui(&srgb);

        assert_eq!(signalled.colour_primaries, 1);
        assert_eq!(signalled.transfer_characteristics, 13);
        assert_eq!(signalled.colour_matrix, 1);
        assert_eq!(signalled.video_full_range_flag, 1);
    }

    #[test]
    fn params_reach_the_rate_control_and_initialisation() {
        let mut tuned = settings(Codec::Av1, PixelFormat::default());
        tuned.params = NvencParams {
            spatial_aq: Some(true),
            aq_strength: Some(9),
            temporal_aq: Some(true),
            multipass: Some(crate::nvenc::params::Multipass::FullResolution),
            split_encode: Some(SplitEncode::Disabled),
            ..NvencParams::default()
        };
        let mut config = configured(&tuned, ALL_FEATURES);
        let flags = config.rc_params.flags;
        let init = initialize_params(&tuned, Resolution::new(1920, 1080), &mut config);

        assert_eq!(rc_bits::ENABLE_AQ.get(flags), 1);
        assert_eq!(rc_bits::AQ_STRENGTH.get(flags), 9);
        assert_eq!(rc_bits::ENABLE_TEMPORAL_AQ.get(flags), 1);
        assert_eq!(config.rc_params.multi_pass, ffi::TWO_PASS_FULL_RESOLUTION);
        assert_eq!(initialize_bits::SPLIT_ENCODE_MODE.get(init.flags), 15);

        let without = configured(&tuned, Features::default());

        assert_eq!(rc_bits::ENABLE_TEMPORAL_AQ.get(without.rc_params.flags), 0);
    }

    #[test]
    fn initialisation_is_synchronous_with_picture_type_decision() {
        let mut low = settings(Codec::Hevc, pixel(ChromaSubsampling::Yuv420, BitDepth::Ten));
        low.frame_rate = FrameRate::new(60_000, 1001);
        low.preset = NvencPreset::P3;
        let mut config = configured(&low, ALL_FEATURES);
        let resolution = Resolution::new(2560, 1440);

        let init = initialize_params(&low, resolution, &mut config);

        assert_eq!(init.version, ffi::INITIALIZE_PARAMS_VER);
        assert_eq!(init.encode_guid, ffi::CODEC_HEVC_GUID);
        assert_eq!(init.preset_guid, ffi::PRESET_P3_GUID);
        assert_eq!((init.encode_width, init.encode_height), (2560, 1440));
        assert_eq!((init.frame_rate_num, init.frame_rate_den), (60_000, 1001));
        assert_eq!(init.enable_encode_async, 0);
        assert_eq!(init.enable_ptd, 1);
        assert_eq!(init.tuning_info, ffi::TUNING_ULTRA_LOW_LATENCY);
        assert_eq!(init.buffer_format, ffi::BUFFER_FORMAT_YUV420_10BIT);
        assert_eq!(init.encode_config, ptr::from_mut(&mut config));
    }

    #[test]
    fn hdr_metadata_becomes_sei_or_metadata_obu_payloads() {
        let hdr = hdr10_color().hdr;

        let sei = hdr_payloads(Codec::Hevc, &hdr);
        let obus = hdr_payloads(Codec::Av1, &hdr);

        assert_eq!(sei.len(), 2);
        assert_eq!(sei[0].kind, 137);
        assert_eq!(sei[0].bytes.len(), 24);
        assert_eq!(
            sei[1],
            Payload {
                kind: 144,
                bytes: vec![0x03, 0xe8, 0x01, 0x90]
            }
        );
        assert_eq!(obus[0].kind, 2);
        assert_eq!(obus[1].kind, 1);
        assert_eq!(
            obus[0].bytes,
            St2086::of(&hdr.mastering.unwrap()).av1_payload().to_vec()
        );
        assert_eq!(hdr_payloads(Codec::H264, &HdrLevels::default()), Vec::new());
    }
}
