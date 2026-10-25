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

const MACROBLOCK_SIZE: u32 = 16;
const LOG2_MAX_FRAME_NUM: u32 = 16;
const DEFAULT_QP: u8 = 26;
const MAX_MV_LENGTH_LOG2: u32 = 15;
const SLICE_TYPE_P: u8 = 0;
const SLICE_TYPE_I: u8 = 2;
const NAL_SLICE: u8 = 1;
const NAL_IDR_SLICE: u8 = 5;
const NAL_SPS: u8 = 7;
const ALL_SLICES_TYPE_OFFSET: u32 = 5;
const NAL_PPS: u8 = 8;
const NAL_REF_IDC_HIGHEST: u8 = 3;
const VIDEO_FORMAT_UNSPECIFIED: u64 = 5;
const ASPECT_RATIO_SQUARE: u8 = 1;
const HIGH_PROFILE_BITRATE_PERCENT: u64 = 125;

const fn level(
    id: u8,
    max_frame_macroblocks: u64,
    max_macroblock_rate: u64,
    max_kbps: u64,
) -> Level {
    let samples = (MACROBLOCK_SIZE * MACROBLOCK_SIZE) as u64;

    Level {
        id,
        max_picture_samples: max_frame_macroblocks * samples,
        max_sample_rate: max_macroblock_rate * samples,
        max_kbps,
        max_kbps_high_tier: None,
    }
}

const INVALID_PICTURE: ffi::VAPictureH264 = ffi::VAPictureH264 {
    picture_id: INVALID_SURFACE,
    frame_idx: 0,
    flags: ffi::VA_PICTURE_H264_INVALID,
    TopFieldOrderCnt: 0,
    BottomFieldOrderCnt: 0,
    va_reserved: [0; 4],
};

const LEVELS: [Level; 19] = [
    level(10, 99, 1_485, 64),
    level(11, 396, 3_000, 192),
    level(12, 396, 6_000, 384),
    level(13, 396, 11_880, 768),
    level(20, 396, 11_880, 2_000),
    level(21, 792, 19_800, 4_000),
    level(22, 1_620, 20_250, 4_000),
    level(30, 1_620, 40_500, 10_000),
    level(31, 3_600, 108_000, 14_000),
    level(32, 5_120, 216_000, 20_000),
    level(40, 8_192, 245_760, 20_000),
    level(41, 8_192, 245_760, 50_000),
    level(42, 8_704, 522_240, 50_000),
    level(50, 22_080, 589_824, 135_000),
    level(51, 36_864, 983_040, 240_000),
    level(52, 36_864, 2_073_600, 240_000),
    level(60, 139_264, 4_177_920, 240_000),
    level(61, 139_264, 8_355_840, 480_000),
    level(62, 139_264, 16_711_680, 800_000),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum H264Profile {
    ConstrainedBaseline,
    Main,
    High,
}

impl H264Profile {
    pub(super) const ORDER: [Self; 3] = [Self::High, Self::Main, Self::ConstrainedBaseline];

    pub(super) const fn va(self) -> Profile {
        match self {
            Self::ConstrainedBaseline => ffi::VAProfile_VAProfileH264ConstrainedBaseline,
            Self::Main => ffi::VAProfile_VAProfileH264Main,
            Self::High => ffi::VAProfile_VAProfileH264High,
        }
    }

    const fn idc(self) -> u8 {
        match self {
            Self::ConstrainedBaseline => 66,
            Self::Main => 77,
            Self::High => 100,
        }
    }

    const fn constraint_flags(self) -> u8 {
        match self {
            Self::ConstrainedBaseline => 0b1100_0000,
            Self::Main => 0b0100_0000,
            Self::High => 0,
        }
    }

    const fn cabac(self) -> bool {
        !matches!(self, Self::ConstrainedBaseline)
    }

    const fn high(self) -> bool {
        matches!(self, Self::High)
    }
}

#[derive(Debug, Clone, Copy)]
struct ReferencePicture {
    surface: u32,
    frame_num: u32,
    order: i32,
}

pub(super) struct H264Stream {
    profile: H264Profile,
    level_idc: u8,
    width_mbs: u32,
    height_mbs: u32,
    crop_right: u32,
    crop_bottom: u32,
    info: StreamInfo,
    frame_num: u32,
    idr_pic_id: u16,
    reference: Option<ReferencePicture>,
}

impl H264Stream {
    pub(super) fn new(profile: H264Profile, info: StreamInfo) -> Self {
        let width_mbs = info.resolution.width.div_ceil(MACROBLOCK_SIZE);
        let height_mbs = info.resolution.height.div_ceil(MACROBLOCK_SIZE);
        let bitrate_percent = if profile.high() {
            HIGH_PROFILE_BITRATE_PERCENT
        } else {
            100
        };
        let demand = Demand::new(info.resolution, info.frame_rate, info.peak_kbps);

        Self {
            profile,
            level_idc: level::select(&LEVELS, demand, bitrate_percent).id,
            width_mbs,
            height_mbs,
            crop_right: (width_mbs * MACROBLOCK_SIZE - info.resolution.width) / 2,
            crop_bottom: (height_mbs * MACROBLOCK_SIZE - info.resolution.height) / 2,
            info,
            frame_num: 0,
            idr_pic_id: 0,
            reference: None,
        }
    }

    pub(super) const fn refresh_block_size() -> u32 {
        MACROBLOCK_SIZE
    }

    fn cropped(&self) -> bool {
        self.crop_right != 0 || self.crop_bottom != 0
    }

    fn qp(&self) -> u8 {
        self.info.qp.unwrap_or(DEFAULT_QP)
    }

    pub(super) fn sps(&self) -> PackedHeader {
        let mut rbsp = BitWriter::new();
        let color = self.info.color;

        rbsp.bits(u64::from(self.profile.idc()), 8);
        rbsp.bits(u64::from(self.profile.constraint_flags()), 8);
        rbsp.bits(u64::from(self.level_idc), 8);
        rbsp.ue(0);
        if self.profile.high() {
            rbsp.ue(1);
            rbsp.ue(0);
            rbsp.ue(0);
            rbsp.flag(false);
            rbsp.flag(false);
        }
        rbsp.ue(LOG2_MAX_FRAME_NUM - 4);
        rbsp.ue(2);
        rbsp.ue(1);
        rbsp.flag(false);
        rbsp.ue(self.width_mbs - 1);
        rbsp.ue(self.height_mbs - 1);
        rbsp.flag(true);
        rbsp.flag(true);
        rbsp.flag(self.cropped());
        if self.cropped() {
            rbsp.ue(0);
            rbsp.ue(self.crop_right);
            rbsp.ue(0);
            rbsp.ue(self.crop_bottom);
        }

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
        rbsp.flag(true);
        rbsp.bits(u64::from(self.info.frame_rate.denominator), 32);
        rbsp.bits(u64::from(self.info.frame_rate.numerator) * 2, 32);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(false);
        rbsp.flag(true);
        rbsp.flag(true);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.ue(MAX_MV_LENGTH_LOG2);
        rbsp.ue(MAX_MV_LENGTH_LOG2);
        rbsp.ue(0);
        rbsp.ue(1);
        rbsp.trailing_bits();

        packed(nal_unit(&[(NAL_REF_IDC_HIGHEST << 5) | NAL_SPS], rbsp))
    }

    pub(super) fn pps(&self) -> PackedHeader {
        let mut rbsp = BitWriter::new();

        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.flag(self.profile.cabac());
        rbsp.flag(false);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.ue(0);
        rbsp.flag(false);
        rbsp.bits(0, 2);
        rbsp.se(i32::from(self.qp()) - 26);
        rbsp.se(0);
        rbsp.se(0);
        rbsp.flag(true);
        rbsp.flag(false);
        rbsp.flag(false);
        if self.profile.high() {
            rbsp.flag(true);
            rbsp.flag(false);
            rbsp.se(0);
        }
        rbsp.trailing_bits();

        packed(nal_unit(&[(NAL_REF_IDC_HIGHEST << 5) | NAL_PPS], rbsp))
    }

    pub(super) fn slice_header(&self, keyframe: bool) -> PackedHeader {
        let mut rbsp = BitWriter::new();
        let slice_type = if keyframe { SLICE_TYPE_I } else { SLICE_TYPE_P };

        rbsp.ue(0);
        rbsp.ue(u32::from(slice_type) + ALL_SLICES_TYPE_OFFSET);
        rbsp.ue(0);
        rbsp.bits(u64::from(self.frame_num), LOG2_MAX_FRAME_NUM);
        if keyframe {
            rbsp.ue(u32::from(self.idr_pic_id));
        } else {
            rbsp.flag(false);
            rbsp.flag(false);
        }
        if keyframe {
            rbsp.flag(false);
            rbsp.flag(false);
        } else {
            rbsp.flag(false);
        }
        if self.profile.cabac() && !keyframe {
            rbsp.ue(0);
        }
        rbsp.se(0);
        rbsp.ue(0);
        rbsp.se(0);
        rbsp.se(0);

        let kind = if keyframe { NAL_IDR_SLICE } else { NAL_SLICE };
        nal_unit_prefix(&[(NAL_REF_IDC_HIGHEST << 5) | kind], rbsp)
    }

    fn sequence_parameters(&self, bits_per_second: u32) -> ffi::VAEncSequenceParameterBufferH264 {
        let mut parameters = ffi::VAEncSequenceParameterBufferH264 {
            level_idc: self.level_idc,
            intra_period: INFINITE_INTRA_PERIOD,
            intra_idr_period: INFINITE_INTRA_PERIOD,
            ip_period: 1,
            bits_per_second,
            max_num_ref_frames: 1,
            picture_width_in_mbs: u16::try_from(self.width_mbs).unwrap_or(u16::MAX),
            picture_height_in_mbs: u16::try_from(self.height_mbs).unwrap_or(u16::MAX),
            frame_cropping_flag: u8::from(self.cropped()),
            frame_crop_right_offset: self.crop_right,
            frame_crop_bottom_offset: self.crop_bottom,
            vui_parameters_present_flag: 1,
            aspect_ratio_idc: ASPECT_RATIO_SQUARE,
            num_units_in_tick: self.info.frame_rate.denominator,
            time_scale: self.info.frame_rate.numerator.saturating_mul(2),
            ..Default::default()
        };

        let sequence = unsafe { &mut parameters.seq_fields.bits };
        sequence.set_chroma_format_idc(1);
        sequence.set_frame_mbs_only_flag(1);
        sequence.set_direct_8x8_inference_flag(1);
        sequence.set_log2_max_frame_num_minus4(LOG2_MAX_FRAME_NUM - 4);
        sequence.set_pic_order_cnt_type(2);

        let vui = unsafe { &mut parameters.vui_fields.bits };
        vui.set_aspect_ratio_info_present_flag(1);
        vui.set_timing_info_present_flag(1);
        vui.set_bitstream_restriction_flag(1);
        vui.set_motion_vectors_over_pic_boundaries_flag(1);
        vui.set_log2_max_mv_length_horizontal(MAX_MV_LENGTH_LOG2);
        vui.set_log2_max_mv_length_vertical(MAX_MV_LENGTH_LOG2);

        parameters
    }

    fn reference_entry(&self) -> Option<ffi::VAPictureH264> {
        self.reference.map(|reference| ffi::VAPictureH264 {
            picture_id: reference.surface,
            frame_idx: reference.frame_num,
            flags: ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE,
            TopFieldOrderCnt: reference.order,
            BottomFieldOrderCnt: reference.order,
            ..Default::default()
        })
    }

    fn picture_parameters(
        &self,
        picture: &Picture,
        order: i32,
    ) -> ffi::VAEncPictureParameterBufferH264 {
        let mut parameters = ffi::VAEncPictureParameterBufferH264 {
            CurrPic: ffi::VAPictureH264 {
                picture_id: picture.recon,
                frame_idx: self.frame_num,
                TopFieldOrderCnt: order,
                BottomFieldOrderCnt: order,
                ..Default::default()
            },
            ReferenceFrames: [INVALID_PICTURE; 16],
            coded_buf: picture.coded,
            frame_num: u16::try_from(self.frame_num).unwrap_or(0),
            pic_init_qp: self.qp(),
            ..Default::default()
        };
        if let Some(reference) = self.reference_entry() {
            parameters.ReferenceFrames[0] = reference;
        }

        let fields = unsafe { &mut parameters.pic_fields.bits };
        fields.set_idr_pic_flag(u32::from(picture.keyframe));
        fields.set_reference_pic_flag(1);
        fields.set_entropy_coding_mode_flag(u32::from(self.profile.cabac()));
        fields.set_transform_8x8_mode_flag(u32::from(self.profile.high()));
        fields.set_deblocking_filter_control_present_flag(1);

        parameters
    }

    fn slice_parameters(&self, picture: &Picture) -> ffi::VAEncSliceParameterBufferH264 {
        let mut slice = ffi::VAEncSliceParameterBufferH264 {
            num_macroblocks: self.width_mbs * self.height_mbs,
            macroblock_info: ffi::VA_INVALID_ID,
            slice_type: if picture.keyframe {
                SLICE_TYPE_I
            } else {
                SLICE_TYPE_P
            },
            idr_pic_id: self.idr_pic_id,
            RefPicList0: [INVALID_PICTURE; 32],
            RefPicList1: [INVALID_PICTURE; 32],
            ..Default::default()
        };
        if let Some(reference) = self.reference_entry() {
            slice.RefPicList0[0] = reference;
        }

        slice
    }

    pub(super) fn buffers(
        &mut self,
        context: &Context,
        picture: &Picture,
    ) -> Result<CodecBuffers, EncodeError> {
        if picture.keyframe {
            self.frame_num = 0;
            self.reference = None;
        }

        let order = i32::try_from((picture.index * 2) % (1 << 31)).unwrap_or(0);
        let sequence = sequence_buffer(context, picture, || {
            self.sequence_parameters(picture.bits_per_second)
        })?;
        let headers = packed_buffers(
            context,
            picture,
            [
                picture.keyframe.then(|| self.sps()),
                picture.keyframe.then(|| self.pps()),
                Some(self.slice_header(picture.keyframe)),
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
            frame_num: self.frame_num,
            order,
        });
        self.frame_num = (self.frame_num + 1) % (1 << LOG2_MAX_FRAME_NUM);
        if picture.keyframe {
            self.idr_pic_id = self.idr_pic_id.wrapping_add(1);
        }

        Ok(buffers)
    }
}
