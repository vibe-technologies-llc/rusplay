use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat};

use crate::{
    EncodeError,
    vaapi::{
        CodecBuffers, INFINITE_INTRA_PERIOD, Picture, StreamInfo,
        bits::{BitWriter, nal_unit, nal_unit_prefix, packed},
        codec_buffers, ffi,
        format::{colour_primaries, full_range, matrix_coefficients, transfer_characteristics},
        level::{self, Demand, Level},
        packed_buffers, sequence_buffer,
        va::{Context, INVALID_SURFACE, PackedHeader, Profile},
    },
};

const LOG2_MIN_CB: u32 = 3;
const LOG2_CTB: u32 = 6;
const LOG2_MIN_TB: u32 = 2;
const LOG2_MAX_TB: u32 = 5;
const MAX_TRANSFORM_HIERARCHY_DEPTH: u32 = 2;
const LOG2_MAX_POC_LSB: u32 = 16;
const MAX_MERGE_CANDIDATES: u8 = 5;
const DEFAULT_QP: u8 = 26;
const MAX_MV_LENGTH_LOG2: u32 = 15;
const NAL_TRAIL_R: u8 = 1;
const NAL_IDR_W_RADL: u8 = 19;
const NAL_VPS: u8 = 32;
const NAL_SPS: u8 = 33;
const NAL_PPS: u8 = 34;
const SLICE_TYPE_P: u8 = 1;
const SLICE_TYPE_I: u8 = 2;
const CODING_TYPE_I: u32 = 1;
const CODING_TYPE_P: u32 = 2;
const VIDEO_FORMAT_UNSPECIFIED: u64 = 5;
const ASPECT_RATIO_SQUARE: u8 = 1;
const NO_COLLOCATED_PICTURE: u8 = 0xff;

const INVALID_PICTURE: ffi::VAPictureHEVC = ffi::VAPictureHEVC {
    picture_id: INVALID_SURFACE,
    pic_order_cnt: 0,
    flags: ffi::VA_PICTURE_HEVC_INVALID,
    va_reserved: [0; 4],
};

const fn level(
    id: u8,
    max_picture_samples: u64,
    max_sample_rate: u64,
    max_kbps: u64,
    max_kbps_high_tier: Option<u64>,
) -> Level {
    Level {
        id,
        max_picture_samples,
        max_sample_rate,
        max_kbps,
        max_kbps_high_tier,
    }
}

const LEVELS: [Level; 13] = [
    level(30, 36_864, 552_960, 128, None),
    level(60, 122_880, 3_686_400, 1_500, None),
    level(63, 245_760, 7_372_800, 3_000, None),
    level(90, 552_960, 16_588_800, 6_000, None),
    level(93, 983_040, 33_177_600, 10_000, None),
    level(120, 2_228_224, 66_846_720, 12_000, Some(30_000)),
    level(123, 2_228_224, 133_693_440, 20_000, Some(50_000)),
    level(150, 8_912_896, 267_386_880, 25_000, Some(100_000)),
    level(153, 8_912_896, 534_773_760, 40_000, Some(160_000)),
    level(156, 8_912_896, 1_069_547_520, 60_000, Some(240_000)),
    level(180, 35_651_584, 1_069_547_520, 60_000, Some(240_000)),
    level(183, 35_651_584, 2_139_095_040, 120_000, Some(480_000)),
    level(186, 35_651_584, 4_278_190_080, 240_000, Some(800_000)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HevcProfile {
    Main,
    Main10,
    Main12,
    Main422_10,
    Main422_12,
    Main444,
    Main444_10,
    Main444_12,
}

impl HevcProfile {
    pub(super) const fn for_format(format: PixelFormat) -> Self {
        match (format.chroma, format.depth) {
            (ChromaSubsampling::Yuv420, BitDepth::Eight) => Self::Main,
            (ChromaSubsampling::Yuv420, BitDepth::Ten) => Self::Main10,
            (ChromaSubsampling::Yuv420, BitDepth::Twelve) => Self::Main12,
            (ChromaSubsampling::Yuv422, BitDepth::Eight | BitDepth::Ten) => Self::Main422_10,
            (ChromaSubsampling::Yuv422, BitDepth::Twelve) => Self::Main422_12,
            (ChromaSubsampling::Yuv444, BitDepth::Eight) => Self::Main444,
            (ChromaSubsampling::Yuv444, BitDepth::Ten) => Self::Main444_10,
            (ChromaSubsampling::Yuv444, BitDepth::Twelve) => Self::Main444_12,
        }
    }

    pub(super) const fn va(self) -> Profile {
        match self {
            Self::Main => ffi::VAProfile_VAProfileHEVCMain,
            Self::Main10 => ffi::VAProfile_VAProfileHEVCMain10,
            Self::Main12 => ffi::VAProfile_VAProfileHEVCMain12,
            Self::Main422_10 => ffi::VAProfile_VAProfileHEVCMain422_10,
            Self::Main422_12 => ffi::VAProfile_VAProfileHEVCMain422_12,
            Self::Main444 => ffi::VAProfile_VAProfileHEVCMain444,
            Self::Main444_10 => ffi::VAProfile_VAProfileHEVCMain444_10,
            Self::Main444_12 => ffi::VAProfile_VAProfileHEVCMain444_12,
        }
    }

    const fn idc(self) -> u8 {
        match self {
            Self::Main => 1,
            Self::Main10 => 2,
            _ => 4,
        }
    }

    const fn compatibility(self) -> u32 {
        match self {
            Self::Main => (1 << (31 - 1)) | (1 << (31 - 2)),
            Self::Main10 => 1 << (31 - 2),
            _ => 1 << (31 - 4),
        }
    }

    const fn range_extension_flags(self) -> Option<[bool; 9]> {
        let (max_12bit, max_10bit, max_8bit, max_422, max_420) = match self {
            Self::Main | Self::Main10 => return None,
            Self::Main12 => (true, false, false, true, true),
            Self::Main422_10 => (true, true, false, true, false),
            Self::Main422_12 => (true, false, false, true, false),
            Self::Main444 => (true, true, true, false, false),
            Self::Main444_10 => (true, true, false, false, false),
            Self::Main444_12 => (true, false, false, false, false),
        };

        Some([
            max_12bit, max_10bit, max_8bit, max_422, max_420, false, false, false, true,
        ])
    }
}

fn small(value: u32) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

const fn chroma_format_idc(chroma: ChromaSubsampling) -> u32 {
    match chroma {
        ChromaSubsampling::Yuv420 => 1,
        ChromaSubsampling::Yuv422 => 2,
        ChromaSubsampling::Yuv444 => 3,
    }
}

const fn chroma_scale(chroma: ChromaSubsampling) -> (u32, u32) {
    match chroma {
        ChromaSubsampling::Yuv420 => (2, 2),
        ChromaSubsampling::Yuv422 => (2, 1),
        ChromaSubsampling::Yuv444 => (1, 1),
    }
}

#[derive(Debug, Clone, Copy)]
struct ReferencePicture {
    surface: u32,
    order: i32,
}

pub(super) struct HevcStream {
    profile: HevcProfile,
    level: level::Selected,
    coded_width: u32,
    coded_height: u32,
    crop_right: u32,
    crop_bottom: u32,
    info: StreamInfo,
    reference: Option<ReferencePicture>,
}

impl HevcStream {
    pub(super) fn new(profile: HevcProfile, info: StreamInfo) -> Self {
        let min_cb = 1 << LOG2_MIN_CB;
        let coded_width = info.resolution.width.next_multiple_of(min_cb);
        let coded_height = info.resolution.height.next_multiple_of(min_cb);
        let (scale_x, scale_y) = chroma_scale(info.format.chroma);
        let demand = Demand::new(info.resolution, info.frame_rate, info.peak_kbps);

        Self {
            profile,
            level: level::select(&LEVELS, demand, 100),
            coded_width,
            coded_height,
            crop_right: (coded_width - info.resolution.width) / scale_x,
            crop_bottom: (coded_height - info.resolution.height) / scale_y,
            info,
            reference: None,
        }
    }

    pub(super) const fn refresh_block_size() -> u32 {
        1 << LOG2_CTB
    }

    fn ctb_count(&self) -> u32 {
        let ctb = 1 << LOG2_CTB;
        self.coded_width.div_ceil(ctb) * self.coded_height.div_ceil(ctb)
    }

    fn cropped(&self) -> bool {
        self.crop_right != 0 || self.crop_bottom != 0
    }

    fn qp(&self) -> u8 {
        self.info.qp.unwrap_or(DEFAULT_QP)
    }

    fn depth_minus8(&self) -> u32 {
        u32::from(self.info.format.depth.bits()) - 8
    }

    fn nal(kind: u8, rbsp: BitWriter) -> Vec<u8> {
        nal_unit(&[kind << 1, 1], rbsp)
    }

    fn profile_tier_level(&self, rbsp: &mut BitWriter) {
        rbsp.bits(0, 2);
        rbsp.flag(self.level.high_tier);
        rbsp.bits(u64::from(self.profile.idc()), 5);
        rbsp.bits(u64::from(self.profile.compatibility()), 32);
        rbsp.flag(true);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(true);
        match self.profile.range_extension_flags() {
            Some(flags) => {
                for flag in flags {
                    rbsp.flag(flag);
                }
                rbsp.bits(0, 34);
            }
            None => rbsp.bits(0, 43),
        }
        rbsp.flag(false);
        rbsp.bits(u64::from(self.level.id), 8);
    }

    fn timing(&self, rbsp: &mut BitWriter) {
        rbsp.bits(u64::from(self.info.frame_rate.denominator), 32);
        rbsp.bits(u64::from(self.info.frame_rate.numerator), 32);
        rbsp.flag(false);
    }

    pub(super) fn vps(&self) -> Vec<u8> {
        let mut rbsp = BitWriter::new();

        rbsp.bits(0, 4);
        rbsp.flag(true);
        rbsp.flag(true);
        rbsp.bits(0, 6);
        rbsp.bits(0, 3);
        rbsp.flag(true);
        rbsp.bits(0xffff, 16);
        self.profile_tier_level(&mut rbsp);
        rbsp.flag(false);
        rbsp.ue(1);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.bits(0, 6);
        rbsp.ue(0);
        rbsp.flag(true);
        self.timing(&mut rbsp);
        rbsp.ue(0);
        rbsp.flag(false);
        rbsp.trailing_bits();

        Self::nal(NAL_VPS, rbsp)
    }

    pub(super) fn sps(&self) -> Vec<u8> {
        let mut rbsp = BitWriter::new();
        let color = self.info.color;
        let chroma_format = chroma_format_idc(self.info.format.chroma);

        rbsp.bits(0, 4);
        rbsp.bits(0, 3);
        rbsp.flag(true);
        self.profile_tier_level(&mut rbsp);
        rbsp.ue(0);
        rbsp.ue(chroma_format);
        if chroma_format == 3 {
            rbsp.flag(false);
        }
        rbsp.ue(self.coded_width);
        rbsp.ue(self.coded_height);
        rbsp.flag(self.cropped());
        if self.cropped() {
            rbsp.ue(0);
            rbsp.ue(self.crop_right);
            rbsp.ue(0);
            rbsp.ue(self.crop_bottom);
        }
        rbsp.ue(self.depth_minus8());
        rbsp.ue(self.depth_minus8());
        rbsp.ue(LOG2_MAX_POC_LSB - 4);
        rbsp.flag(true);
        rbsp.ue(1);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.ue(LOG2_MIN_CB - 3);
        rbsp.ue(LOG2_CTB - LOG2_MIN_CB);
        rbsp.ue(LOG2_MIN_TB - 2);
        rbsp.ue(LOG2_MAX_TB - LOG2_MIN_TB);
        rbsp.ue(MAX_TRANSFORM_HIERARCHY_DEPTH);
        rbsp.ue(MAX_TRANSFORM_HIERARCHY_DEPTH);
        rbsp.flag(false);
        rbsp.flag(true);
        rbsp.flag(true);
        rbsp.flag(false);
        rbsp.ue(1);
        rbsp.ue(1);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.flag(true);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);

        rbsp.flag(true);
        rbsp.flag(true);
        rbsp.bits(u64::from(ASPECT_RATIO_SQUARE), 8);
        rbsp.flag(false);
        rbsp.flag(true);
        rbsp.bits(VIDEO_FORMAT_UNSPECIFIED, 3);
        rbsp.flag(full_range(color.range));
        rbsp.flag(true);
        rbsp.bits(u64::from(colour_primaries(color.primaries)), 8);
        rbsp.bits(u64::from(transfer_characteristics(color.transfer)), 8);
        rbsp.bits(u64::from(matrix_coefficients(color.primaries)), 8);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(true);
        self.timing(&mut rbsp);
        rbsp.flag(false);
        rbsp.flag(true);
        rbsp.flag(false);
        rbsp.flag(true);
        rbsp.flag(true);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.ue(MAX_MV_LENGTH_LOG2);
        rbsp.ue(MAX_MV_LENGTH_LOG2);
        rbsp.flag(false);
        rbsp.trailing_bits();

        Self::nal(NAL_SPS, rbsp)
    }

    pub(super) fn pps(&self) -> Vec<u8> {
        let mut rbsp = BitWriter::new();

        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.bits(0, 3);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.se(i32::from(self.qp()) - 26);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(true);
        rbsp.ue(0);
        rbsp.se(0);
        rbsp.se(0);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(true);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.ue(0);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.trailing_bits();

        Self::nal(NAL_PPS, rbsp)
    }

    pub(super) fn slice_header(keyframe: bool, order: i32) -> PackedHeader {
        let mut rbsp = BitWriter::new();

        rbsp.flag(true);
        if keyframe {
            rbsp.flag(false);
        }
        rbsp.ue(0);
        rbsp.ue(u32::from(if keyframe {
            SLICE_TYPE_I
        } else {
            SLICE_TYPE_P
        }));
        if !keyframe {
            rbsp.bits(u64::try_from(order).unwrap_or(0), LOG2_MAX_POC_LSB);
            rbsp.flag(true);
        }
        rbsp.flag(true);
        rbsp.flag(true);
        if !keyframe {
            rbsp.flag(false);
            rbsp.ue(5 - u32::from(MAX_MERGE_CANDIDATES));
        }
        rbsp.se(0);
        rbsp.flag(true);
        rbsp.trailing_bits();

        let kind = if keyframe {
            NAL_IDR_W_RADL
        } else {
            NAL_TRAIL_R
        };
        nal_unit_prefix(&[kind << 1, 1], rbsp)
    }

    pub(super) fn sequence_header(&self) -> PackedHeader {
        let mut bytes = self.vps();
        bytes.extend(self.sps());

        packed(bytes)
    }

    fn sequence_parameters(&self, bits_per_second: u32) -> ffi::VAEncSequenceParameterBufferHEVC {
        let mut parameters = ffi::VAEncSequenceParameterBufferHEVC {
            general_profile_idc: self.profile.idc(),
            general_level_idc: self.level.id,
            general_tier_flag: u8::from(self.level.high_tier),
            intra_period: INFINITE_INTRA_PERIOD,
            intra_idr_period: INFINITE_INTRA_PERIOD,
            ip_period: 1,
            bits_per_second,
            pic_width_in_luma_samples: u16::try_from(self.coded_width).unwrap_or(u16::MAX),
            pic_height_in_luma_samples: u16::try_from(self.coded_height).unwrap_or(u16::MAX),
            log2_min_luma_coding_block_size_minus3: small(LOG2_MIN_CB - 3),
            log2_diff_max_min_luma_coding_block_size: small(LOG2_CTB - LOG2_MIN_CB),
            log2_min_transform_block_size_minus2: small(LOG2_MIN_TB - 2),
            log2_diff_max_min_transform_block_size: small(LOG2_MAX_TB - LOG2_MIN_TB),
            max_transform_hierarchy_depth_inter: small(MAX_TRANSFORM_HIERARCHY_DEPTH),
            max_transform_hierarchy_depth_intra: small(MAX_TRANSFORM_HIERARCHY_DEPTH),
            vui_parameters_present_flag: 1,
            aspect_ratio_idc: ASPECT_RATIO_SQUARE,
            vui_num_units_in_tick: self.info.frame_rate.denominator,
            vui_time_scale: self.info.frame_rate.numerator,
            ..Default::default()
        };

        let sequence = unsafe { &mut parameters.seq_fields.bits };
        sequence.set_chroma_format_idc(chroma_format_idc(self.info.format.chroma));
        sequence.set_bit_depth_luma_minus8(self.depth_minus8());
        sequence.set_bit_depth_chroma_minus8(self.depth_minus8());
        sequence.set_amp_enabled_flag(1);
        sequence.set_sample_adaptive_offset_enabled_flag(1);
        sequence.set_low_delay_seq(1);

        let vui = unsafe { &mut parameters.vui_fields.bits };
        vui.set_aspect_ratio_info_present_flag(1);
        vui.set_vui_timing_info_present_flag(1);
        vui.set_bitstream_restriction_flag(1);
        vui.set_motion_vectors_over_pic_boundaries_flag(1);
        vui.set_restricted_ref_pic_lists_flag(1);
        vui.set_log2_max_mv_length_horizontal(MAX_MV_LENGTH_LOG2);
        vui.set_log2_max_mv_length_vertical(MAX_MV_LENGTH_LOG2);

        parameters
    }

    fn reference_entry(&self) -> Option<ffi::VAPictureHEVC> {
        self.reference.map(|reference| ffi::VAPictureHEVC {
            picture_id: reference.surface,
            pic_order_cnt: reference.order,
            flags: ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE,
            ..Default::default()
        })
    }

    fn picture_parameters(
        &self,
        picture: &Picture,
        order: i32,
    ) -> ffi::VAEncPictureParameterBufferHEVC {
        let mut parameters = ffi::VAEncPictureParameterBufferHEVC {
            decoded_curr_pic: ffi::VAPictureHEVC {
                picture_id: picture.recon,
                pic_order_cnt: order,
                ..Default::default()
            },
            reference_frames: [INVALID_PICTURE; 15],
            coded_buf: picture.coded,
            collocated_ref_pic_index: NO_COLLOCATED_PICTURE,
            pic_init_qp: self.qp(),
            nal_unit_type: if picture.keyframe {
                NAL_IDR_W_RADL
            } else {
                NAL_TRAIL_R
            },
            ..Default::default()
        };
        if let Some(reference) = self.reference_entry() {
            parameters.reference_frames[0] = reference;
        }

        let fields = unsafe { &mut parameters.pic_fields.bits };
        fields.set_idr_pic_flag(u32::from(picture.keyframe));
        fields.set_coding_type(if picture.keyframe {
            CODING_TYPE_I
        } else {
            CODING_TYPE_P
        });
        fields.set_reference_pic_flag(1);
        fields.set_cu_qp_delta_enabled_flag(1);
        fields.set_pps_loop_filter_across_slices_enabled_flag(1);

        parameters
    }

    fn slice_parameters(&self, picture: &Picture) -> ffi::VAEncSliceParameterBufferHEVC {
        let mut slice = ffi::VAEncSliceParameterBufferHEVC {
            num_ctu_in_slice: self.ctb_count(),
            slice_type: if picture.keyframe {
                SLICE_TYPE_I
            } else {
                SLICE_TYPE_P
            },
            ref_pic_list0: [INVALID_PICTURE; 15],
            ref_pic_list1: [INVALID_PICTURE; 15],
            max_num_merge_cand: MAX_MERGE_CANDIDATES,
            ..Default::default()
        };
        if let Some(reference) = self.reference_entry() {
            slice.ref_pic_list0[0] = reference;
        }

        let fields = unsafe { &mut slice.slice_fields.bits };
        fields.set_last_slice_of_pic_flag(1);
        fields.set_slice_sao_luma_flag(1);
        fields.set_slice_sao_chroma_flag(1);
        fields.set_slice_loop_filter_across_slices_enabled_flag(1);

        slice
    }

    pub(super) fn buffers(
        &mut self,
        context: &Context,
        picture: &Picture,
    ) -> Result<CodecBuffers, EncodeError> {
        if picture.keyframe {
            self.reference = None;
        }

        let order = i32::try_from(picture.index % (1 << LOG2_MAX_POC_LSB)).unwrap_or(0);
        let sequence = sequence_buffer(context, picture, || {
            self.sequence_parameters(picture.bits_per_second)
        })?;
        let headers = packed_buffers(
            context,
            picture,
            [
                picture.keyframe.then(|| self.sequence_header()),
                picture.keyframe.then(|| packed(self.pps())),
                Some(Self::slice_header(picture.keyframe, order)),
            ],
        )?;
        let buffers = codec_buffers(
            context,
            sequence,
            &self.picture_parameters(picture, order),
            headers,
            &self.slice_parameters(picture),
        )?;

        self.reference = Some(ReferencePicture {
            surface: picture.recon,
            order,
        });

        Ok(buffers)
    }
}
