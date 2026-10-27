use std::ffi::{c_char, c_int, c_uint, c_void};

pub(super) const API_MAJOR: u32 = 13;
pub(super) const API_MINOR: u32 = 0;
pub(super) const API_VERSION: u32 = API_MAJOR | (API_MINOR << 24);
pub(super) const MAX_SUPPORTED_VERSION: u32 = (API_MAJOR << 4) | API_MINOR;

const STRUCT_VERSION_TAG: u32 = 0x7 << 28;
const WIDE_STRUCT: u32 = 1 << 31;

pub(super) const fn struct_version(version: u32) -> u32 {
    API_VERSION | (version << 16) | STRUCT_VERSION_TAG
}

pub(super) const FUNCTION_LIST_VER: u32 = struct_version(2);
pub(super) const OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = struct_version(1);
pub(super) const CAPS_PARAM_VER: u32 = struct_version(1);
pub(super) const RC_PARAMS_VER: u32 = struct_version(1);
pub(super) const CONFIG_VER: u32 = struct_version(9) | WIDE_STRUCT;
pub(super) const PRESET_CONFIG_VER: u32 = struct_version(5) | WIDE_STRUCT;
pub(super) const INITIALIZE_PARAMS_VER: u32 = struct_version(7) | WIDE_STRUCT;
pub(super) const RECONFIGURE_PARAMS_VER: u32 = struct_version(2) | WIDE_STRUCT;
pub(super) const CREATE_INPUT_BUFFER_VER: u32 = struct_version(2);
pub(super) const LOCK_INPUT_BUFFER_VER: u32 = struct_version(1);
pub(super) const CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
pub(super) const LOCK_BITSTREAM_VER: u32 = struct_version(2) | WIDE_STRUCT;
pub(super) const PIC_PARAMS_VER: u32 = struct_version(7) | WIDE_STRUCT;
pub(super) const REGISTER_RESOURCE_VER: u32 = struct_version(5);
pub(super) const MAP_INPUT_RESOURCE_VER: u32 = struct_version(4);

pub(super) const INFINITE_GOP_LENGTH: u32 = 0xffff_ffff;

pub(super) const SUCCESS: i32 = 0;
pub(super) const DEVICE_TYPE_CUDA: u32 = 1;

pub(super) const RC_CONSTQP: u32 = 0;
pub(super) const RC_VBR: u32 = 1;
pub(super) const RC_CBR: u32 = 2;

pub(super) const MULTI_PASS_DISABLED: u32 = 0;
pub(super) const TWO_PASS_QUARTER_RESOLUTION: u32 = 1;
pub(super) const TWO_PASS_FULL_RESOLUTION: u32 = 2;

pub(super) const PIC_STRUCT_FRAME: u32 = 1;
pub(super) const PIC_TYPE_IDR: u32 = 3;
pub(super) const PIC_FLAG_FORCE_IDR: u32 = 0x2;
pub(super) const PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x4;

pub(super) const BUFFER_FORMAT_NV12: u32 = 0x0000_0001;
pub(super) const BUFFER_FORMAT_YUV444: u32 = 0x0000_1000;
pub(super) const BUFFER_FORMAT_YUV420_10BIT: u32 = 0x0001_0000;
pub(super) const BUFFER_FORMAT_YUV444_10BIT: u32 = 0x0010_0000;
pub(super) const BUFFER_FORMAT_NV16: u32 = 0x4000_0001;
pub(super) const BUFFER_FORMAT_P210: u32 = 0x4000_0002;

pub(super) const MEMORY_HEAP_AUTOSELECT: u32 = 0;
pub(super) const INPUT_RESOURCE_TYPE_CUDADEVICEPTR: u32 = 1;
pub(super) const BUFFER_USAGE_INPUT_IMAGE: u32 = 0;

pub(super) const CAPS_SUPPORTED_RATECONTROL_MODES: u32 = 1;
pub(super) const CAPS_WIDTH_MAX: u32 = 16;
pub(super) const CAPS_HEIGHT_MAX: u32 = 17;
pub(super) const CAPS_SUPPORT_INTRA_REFRESH: u32 = 25;
pub(super) const CAPS_SUPPORT_YUV444_ENCODE: u32 = 33;
pub(super) const CAPS_SUPPORT_TEMPORAL_AQ: u32 = 38;
pub(super) const CAPS_SUPPORT_10BIT_ENCODE: u32 = 39;
pub(super) const CAPS_WIDTH_MIN: u32 = 45;
pub(super) const CAPS_HEIGHT_MIN: u32 = 46;
pub(super) const CAPS_SUPPORT_YUV422_ENCODE: u32 = 59;

pub(super) const TUNING_HIGH_QUALITY: u32 = 1;
pub(super) const TUNING_LOW_LATENCY: u32 = 2;
pub(super) const TUNING_ULTRA_LOW_LATENCY: u32 = 3;

pub(super) const SPLIT_AUTO_MODE: u32 = 0;
pub(super) const SPLIT_AUTO_FORCED_MODE: u32 = 1;
pub(super) const SPLIT_TWO_FORCED_MODE: u32 = 2;
pub(super) const SPLIT_THREE_FORCED_MODE: u32 = 3;
pub(super) const SPLIT_FOUR_FORCED_MODE: u32 = 4;
pub(super) const SPLIT_DISABLE_MODE: u32 = 15;

pub(super) const VUI_VIDEO_FORMAT_UNSPECIFIED: u32 = 5;

pub(super) const BIT_DEPTH_8: u32 = 8;
pub(super) const BIT_DEPTH_10: u32 = 10;

pub(super) const CHROMA_FORMAT_IDC_420: u32 = 1;
pub(super) const CHROMA_FORMAT_IDC_422: u32 = 2;
pub(super) const CHROMA_FORMAT_IDC_444: u32 = 3;

pub(super) const AV1_COLOR_RANGE_STUDIO: u32 = 0;
pub(super) const AV1_COLOR_RANGE_FULL: u32 = 1;

pub(super) const CUDA_SUCCESS: i32 = 0;
pub(super) const CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD: u32 = 1;
pub(super) const CUDA_EXTERNAL_MEMORY_DEDICATED: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub(super) struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }
}

pub(super) const CODEC_H264_GUID: Guid = Guid::new(
    0x6bc8_2762,
    0x4e63,
    0x4ca4,
    [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
);
pub(super) const CODEC_HEVC_GUID: Guid = Guid::new(
    0x790c_dc88,
    0x4522,
    0x4d7b,
    [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
);
pub(super) const CODEC_AV1_GUID: Guid = Guid::new(
    0x0a35_2289,
    0x0aa7,
    0x4759,
    [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
);
pub(super) const H264_PROFILE_HIGH_GUID: Guid = Guid::new(
    0xe7cb_c309,
    0x4f7a,
    0x4b89,
    [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
);
pub(super) const H264_PROFILE_HIGH_10_GUID: Guid = Guid::new(
    0x8f0c_337e,
    0x186c,
    0x48e9,
    [0xa6, 0x9d, 0x7a, 0x83, 0x34, 0x08, 0x97, 0x58],
);
pub(super) const H264_PROFILE_HIGH_422_GUID: Guid = Guid::new(
    0xff32_42e9,
    0x613c,
    0x4295,
    [0xa1, 0xe8, 0x2a, 0x7f, 0xe9, 0x4d, 0x81, 0x33],
);
pub(super) const H264_PROFILE_HIGH_444_GUID: Guid = Guid::new(
    0x7ac6_63cb,
    0xa598,
    0x4960,
    [0xb8, 0x44, 0x33, 0x9b, 0x26, 0x1a, 0x7d, 0x52],
);
pub(super) const HEVC_PROFILE_MAIN_GUID: Guid = Guid::new(
    0xb514_c39a,
    0xb55b,
    0x40fa,
    [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
);
pub(super) const HEVC_PROFILE_MAIN10_GUID: Guid = Guid::new(
    0xfa4d_2b6c,
    0x3a5b,
    0x411a,
    [0x80, 0x18, 0x0a, 0x3f, 0x5e, 0x3c, 0x9b, 0xe5],
);
pub(super) const HEVC_PROFILE_FREXT_GUID: Guid = Guid::new(
    0x51ec_32b5,
    0x1b4c,
    0x453c,
    [0x9c, 0xbd, 0xb6, 0x16, 0xbd, 0x62, 0x13, 0x41],
);
pub(super) const AV1_PROFILE_MAIN_GUID: Guid = Guid::new(
    0x5f2a_39f5,
    0xf14e,
    0x4f95,
    [0x9a, 0x9e, 0xb7, 0x6d, 0x56, 0x8f, 0xcf, 0x97],
);
pub(super) const PRESET_P1_GUID: Guid = Guid::new(
    0xfc0a_8d3e,
    0x45f8,
    0x4cf8,
    [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
);
pub(super) const PRESET_P2_GUID: Guid = Guid::new(
    0xf581_cfb8,
    0x88d6,
    0x4381,
    [0x93, 0xf0, 0xdf, 0x13, 0xf9, 0xc2, 0x7d, 0xab],
);
pub(super) const PRESET_P3_GUID: Guid = Guid::new(
    0x3685_0110,
    0x3a07,
    0x441f,
    [0x94, 0xd5, 0x36, 0x70, 0x63, 0x1f, 0x91, 0xf6],
);
pub(super) const PRESET_P4_GUID: Guid = Guid::new(
    0x90a7_b826,
    0xdf06,
    0x4862,
    [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
);
pub(super) const PRESET_P5_GUID: Guid = Guid::new(
    0x21c6_e6b4,
    0x297a,
    0x4cba,
    [0x99, 0x8f, 0xb6, 0xcb, 0xde, 0x72, 0xad, 0xe3],
);
pub(super) const PRESET_P6_GUID: Guid = Guid::new(
    0x8e75_c279,
    0x6299,
    0x4ab6,
    [0x83, 0x02, 0x0b, 0x21, 0x5a, 0x33, 0x5c, 0xf5],
);
pub(super) const PRESET_P7_GUID: Guid = Guid::new(
    0x8484_8c12,
    0x6f71,
    0x4c13,
    [0x93, 0x1b, 0x53, 0xe2, 0x83, 0xf5, 0x79, 0x74],
);

pub(super) trait Plain: Sized {
    fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BitField {
    pub offset: u32,
    pub width: u32,
}

impl BitField {
    const fn flag(offset: u32) -> Self {
        Self { offset, width: 1 }
    }

    const fn mask(self) -> u32 {
        u32::MAX >> (32 - self.width)
    }

    pub(super) fn set(self, word: &mut u32, value: u32) {
        let mask = self.mask() << self.offset;
        *word = (*word & !mask) | ((value << self.offset) & mask);
    }

    #[cfg(test)]
    pub(super) const fn get(self, word: u32) -> u32 {
        (word >> self.offset) & self.mask()
    }
}

pub(super) mod rc_bits {
    use super::BitField;

    pub(in crate::nvenc) const ENABLE_AQ: BitField = BitField::flag(3);
    pub(in crate::nvenc) const ENABLE_LOOKAHEAD: BitField = BitField::flag(5);
    pub(in crate::nvenc) const ENABLE_TEMPORAL_AQ: BitField = BitField::flag(8);
    pub(in crate::nvenc) const ZERO_REORDER_DELAY: BitField = BitField::flag(9);
    pub(in crate::nvenc) const AQ_STRENGTH: BitField = BitField {
        offset: 12,
        width: 4,
    };
}

pub(super) mod h264_bits {
    use super::BitField;

    pub(in crate::nvenc) const OUTPUT_AUD: BitField = BitField::flag(6);
    pub(in crate::nvenc) const ENABLE_INTRA_REFRESH: BitField = BitField::flag(10);
    pub(in crate::nvenc) const REPEAT_SPSPPS: BitField = BitField::flag(12);
}

pub(super) mod hevc_bits {
    use super::BitField;

    pub(in crate::nvenc) const OUTPUT_AUD: BitField = BitField::flag(4);
    pub(in crate::nvenc) const REPEAT_SPSPPS: BitField = BitField::flag(7);
    pub(in crate::nvenc) const ENABLE_INTRA_REFRESH: BitField = BitField::flag(8);
    pub(in crate::nvenc) const CHROMA_FORMAT_IDC: BitField = BitField {
        offset: 9,
        width: 2,
    };
    pub(in crate::nvenc) const OUTPUT_MAX_CLL: BitField = BitField::flag(23);
    pub(in crate::nvenc) const OUTPUT_MASTERING_DISPLAY: BitField = BitField::flag(24);
}

pub(super) mod av1_bits {
    use super::BitField;

    pub(in crate::nvenc) const REPEAT_SEQ_HDR: BitField = BitField::flag(5);
    pub(in crate::nvenc) const ENABLE_INTRA_REFRESH: BitField = BitField::flag(6);
    pub(in crate::nvenc) const CHROMA_FORMAT_IDC: BitField = BitField {
        offset: 7,
        width: 2,
    };
    pub(in crate::nvenc) const OUTPUT_MAX_CLL: BitField = BitField::flag(14);
    pub(in crate::nvenc) const OUTPUT_MASTERING_DISPLAY: BitField = BitField::flag(15);
}

pub(super) mod initialize_bits {
    use super::BitField;

    pub(in crate::nvenc) const SPLIT_ENCODE_MODE: BitField = BitField {
        offset: 5,
        width: 4,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub(super) struct Qp {
    pub inter_p: u32,
    pub inter_b: u32,
    pub intra: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct RcParams {
    pub version: u32,
    pub rate_control_mode: u32,
    pub const_qp: Qp,
    pub average_bit_rate: u32,
    pub max_bit_rate: u32,
    pub vbv_buffer_size: u32,
    pub vbv_initial_delay: u32,
    pub flags: u32,
    pub min_qp: Qp,
    pub max_qp: Qp,
    pub initial_rc_qp: Qp,
    pub temporal_layer_idx_mask: u32,
    pub temporal_layer_qp: [u8; 8],
    pub target_quality: u8,
    pub target_quality_lsb: u8,
    pub lookahead_depth: u16,
    pub low_delay_key_frame_scale: u8,
    pub y_dc_qp_index_offset: i8,
    pub u_dc_qp_index_offset: i8,
    pub v_dc_qp_index_offset: i8,
    pub qp_map_mode: u32,
    pub multi_pass: u32,
    pub alpha_layer_bitrate_ratio: u32,
    pub cb_qp_index_offset: i8,
    pub cr_qp_index_offset: i8,
    pub reserved2: u16,
    pub lookahead_level: u32,
    pub view_bitrate_ratios: [u8; 7],
    pub reserved3: u8,
    pub reserved1: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub(super) struct VuiParameters {
    pub overscan_info_present_flag: u32,
    pub overscan_info: u32,
    pub video_signal_type_present_flag: u32,
    pub video_format: u32,
    pub video_full_range_flag: u32,
    pub colour_description_present_flag: u32,
    pub colour_primaries: u32,
    pub transfer_characteristics: u32,
    pub colour_matrix: u32,
    pub chroma_sample_location_flag: u32,
    pub chroma_sample_location_top: u32,
    pub chroma_sample_location_bot: u32,
    pub bitstream_restriction_flag: u32,
    pub timing_info_present_flag: u32,
    pub num_unit_in_ticks: u32,
    pub time_scale: u32,
    pub reserved: [u32; 12],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ConfigH264 {
    pub flags: u32,
    pub level: u32,
    pub idr_period: u32,
    pub separate_colour_plane_flag: u32,
    pub disable_deblocking_filter_idc: u32,
    pub num_temporal_layers: u32,
    pub sps_id: u32,
    pub pps_id: u32,
    pub adaptive_transform_mode: u32,
    pub fmo_mode: u32,
    pub bdirect_mode: u32,
    pub entropy_coding_mode: u32,
    pub stereo_mode: u32,
    pub intra_refresh_period: u32,
    pub intra_refresh_cnt: u32,
    pub max_num_ref_frames: u32,
    pub slice_mode: u32,
    pub slice_mode_data: u32,
    pub vui: VuiParameters,
    pub ltr_num_frames: u32,
    pub ltr_trust_mode: u32,
    pub chroma_format_idc: u32,
    pub max_temporal_layers: u32,
    pub use_b_frames_as_ref: u32,
    pub num_ref_l0: u32,
    pub num_ref_l1: u32,
    pub output_bit_depth: u32,
    pub input_bit_depth: u32,
    pub tf_level: u32,
    pub reserved1: [u32; 264],
    pub reserved2: [*mut c_void; 64],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ConfigHevc {
    pub level: u32,
    pub tier: u32,
    pub min_cu_size: u32,
    pub max_cu_size: u32,
    pub flags: u32,
    pub idr_period: u32,
    pub intra_refresh_period: u32,
    pub intra_refresh_cnt: u32,
    pub max_num_ref_frames_in_dpb: u32,
    pub ltr_num_frames: u32,
    pub vps_id: u32,
    pub sps_id: u32,
    pub pps_id: u32,
    pub slice_mode: u32,
    pub slice_mode_data: u32,
    pub max_temporal_layers_minus1: u32,
    pub vui: VuiParameters,
    pub ltr_trust_mode: u32,
    pub use_b_frames_as_ref: u32,
    pub num_ref_l0: u32,
    pub num_ref_l1: u32,
    pub tf_level: u32,
    pub disable_deblocking_filter_idc: u32,
    pub output_bit_depth: u32,
    pub input_bit_depth: u32,
    pub num_temporal_layers: u32,
    pub num_views: u32,
    pub reserved1: [u32; 208],
    pub reserved2: [*mut c_void; 64],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ConfigAv1 {
    pub level: u32,
    pub tier: u32,
    pub min_part_size: u32,
    pub max_part_size: u32,
    pub flags: u32,
    pub idr_period: u32,
    pub intra_refresh_period: u32,
    pub intra_refresh_cnt: u32,
    pub max_num_ref_frames_in_dpb: u32,
    pub num_tile_columns: u32,
    pub num_tile_rows: u32,
    pub reserved2: u32,
    pub tile_widths: *mut u32,
    pub tile_heights: *mut u32,
    pub max_temporal_layers_minus1: u32,
    pub color_primaries: u32,
    pub transfer_characteristics: u32,
    pub matrix_coefficients: u32,
    pub color_range: u32,
    pub chroma_sample_position: u32,
    pub use_b_frames_as_ref: u32,
    pub film_grain_params: *mut c_void,
    pub num_fwd_refs: u32,
    pub num_bwd_refs: u32,
    pub output_bit_depth: u32,
    pub input_bit_depth: u32,
    pub ltr_num_frames: u32,
    pub num_temporal_layers: u32,
    pub tf_level: u32,
    pub reserved1: [u32; 230],
    pub reserved3: [*mut c_void; 62],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) union CodecConfig {
    pub h264: ConfigH264,
    pub hevc: ConfigHevc,
    pub av1: ConfigAv1,
    pub reserved: [u32; 320],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct Config {
    pub version: u32,
    pub profile_guid: Guid,
    pub gop_length: u32,
    pub frame_interval_p: i32,
    pub mono_chrome_encoding: u32,
    pub frame_field_mode: u32,
    pub mv_precision: u32,
    pub rc_params: RcParams,
    pub codec: CodecConfig,
    pub reserved: [u32; 278],
    pub reserved2: [*mut c_void; 64],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PresetConfig {
    pub version: u32,
    pub reserved: u32,
    pub preset_cfg: Config,
    pub reserved1: [u32; 256],
    pub reserved2: [*mut c_void; 64],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct InitializeParams {
    pub version: u32,
    pub encode_guid: Guid,
    pub preset_guid: Guid,
    pub encode_width: u32,
    pub encode_height: u32,
    pub dar_width: u32,
    pub dar_height: u32,
    pub frame_rate_num: u32,
    pub frame_rate_den: u32,
    pub enable_encode_async: u32,
    pub enable_ptd: u32,
    pub flags: u32,
    pub priv_data_size: u32,
    pub reserved: u32,
    pub priv_data: *mut c_void,
    pub encode_config: *mut Config,
    pub max_encode_width: u32,
    pub max_encode_height: u32,
    pub max_me_hint_counts_per_block: [[u32; 4]; 2],
    pub tuning_info: u32,
    pub buffer_format: u32,
    pub num_state_buffers: u32,
    pub output_stats_level: u32,
    pub reserved1: [u32; 284],
    pub reserved2: [*mut c_void; 64],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ReconfigureParams {
    pub version: u32,
    pub reserved: u32,
    pub re_init_encode_params: InitializeParams,
    pub flags: u32,
    pub reserved2: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CapsParam {
    pub version: u32,
    pub caps_to_query: u32,
    pub reserved: [u32; 62],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct OpenEncodeSessionExParams {
    pub version: u32,
    pub device_type: u32,
    pub device: *mut c_void,
    pub reserved: *mut c_void,
    pub api_version: u32,
    pub reserved1: [u32; 253],
    pub reserved2: [*mut c_void; 64],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CreateInputBuffer {
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub memory_heap: u32,
    pub buffer_fmt: u32,
    pub reserved: u32,
    pub input_buffer: *mut c_void,
    pub sys_mem_buffer: *mut c_void,
    pub reserved1: [u32; 58],
    pub reserved2: [*mut c_void; 63],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct LockInputBuffer {
    pub version: u32,
    pub flags: u32,
    pub input_buffer: *mut c_void,
    pub buffer_data_ptr: *mut c_void,
    pub pitch: u32,
    pub reserved1: [u32; 251],
    pub reserved2: [*mut c_void; 64],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CreateBitstreamBuffer {
    pub version: u32,
    pub size: u32,
    pub memory_heap: u32,
    pub reserved: u32,
    pub bitstream_buffer: *mut c_void,
    pub bitstream_buffer_ptr: *mut c_void,
    pub reserved1: [u32; 58],
    pub reserved2: [*mut c_void; 64],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct LockBitstream {
    pub version: u32,
    pub flags: u32,
    pub output_bitstream: *mut c_void,
    pub slice_offsets: *mut u32,
    pub frame_idx: u32,
    pub hw_encode_status: u32,
    pub num_slices: u32,
    pub bitstream_size_in_bytes: u32,
    pub output_time_stamp: u64,
    pub output_duration: u64,
    pub bitstream_buffer_ptr: *mut c_void,
    pub picture_type: u32,
    pub picture_struct: u32,
    pub frame_avg_qp: u32,
    pub frame_satd: u32,
    pub ltr_frame_idx: u32,
    pub ltr_frame_bitmap: u32,
    pub temporal_id: u32,
    pub intra_mb_count: u32,
    pub inter_mb_count: u32,
    pub average_mvx: i32,
    pub average_mvy: i32,
    pub alpha_layer_size_in_bytes: u32,
    pub output_stats_ptr_size: u32,
    pub reserved: u32,
    pub output_stats_ptr: *mut c_void,
    pub frame_idx_display: u32,
    pub reserved1: [u32; 219],
    pub reserved2: [*mut c_void; 63],
    pub reserved_internal: [u32; 8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub(super) struct ChromaPoints {
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub(super) struct MasteringDisplayInfo {
    pub g: ChromaPoints,
    pub b: ChromaPoints,
    pub r: ChromaPoints,
    pub white_point: ChromaPoints,
    pub max_luma: u32,
    pub min_luma: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub(super) struct ContentLightLevel {
    pub max_content_light_level: u16,
    pub max_pic_average_light_level: u16,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub(super) struct SeiPayload {
    pub payload_size: u32,
    pub payload_type: u32,
    pub payload: *mut u8,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PicParamsH264 {
    pub display_poc_syntax: u32,
    pub reserved3: u32,
    pub ref_pic_flag: u32,
    pub colour_plane_id: u32,
    pub force_intra_refresh_with_frame_cnt: u32,
    pub flags: u32,
    pub slice_type_data: *mut u8,
    pub slice_type_array_cnt: u32,
    pub sei_payload_array_cnt: u32,
    pub sei_payload_array: *mut SeiPayload,
    pub slice_mode: u32,
    pub slice_mode_data: u32,
    pub ltr_mark_frame_idx: u32,
    pub ltr_use_frame_bitmap: u32,
    pub ltr_usage_mode: u32,
    pub force_intra_slice_count: u32,
    pub force_intra_slice_idx: *mut u32,
    pub h264_ext_pic_params: [u64; 16],
    pub time_code: [u32; 8],
    pub reserved: [u32; 202],
    pub reserved2: [*mut c_void; 61],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PicParamsHevc {
    pub display_poc_syntax: u32,
    pub ref_pic_flag: u32,
    pub temporal_id: u32,
    pub force_intra_refresh_with_frame_cnt: u32,
    pub flags: u32,
    pub reserved1: u32,
    pub slice_type_data: *mut u8,
    pub slice_type_array_cnt: u32,
    pub slice_mode: u32,
    pub slice_mode_data: u32,
    pub ltr_mark_frame_idx: u32,
    pub ltr_use_frame_bitmap: u32,
    pub ltr_usage_mode: u32,
    pub sei_payload_array_cnt: u32,
    pub reserved: u32,
    pub sei_payload_array: *mut SeiPayload,
    pub time_code: [u32; 8],
    pub num_temporal_layers: u32,
    pub view_id: u32,
    pub reference_display_info_3d: *mut c_void,
    pub max_cll: *mut ContentLightLevel,
    pub mastering_display: *mut MasteringDisplayInfo,
    pub reserved2: [u32; 234],
    pub reserved3: [*mut c_void; 58],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PicParamsAv1 {
    pub display_poc_syntax: u32,
    pub ref_pic_flag: u32,
    pub temporal_id: u32,
    pub force_intra_refresh_with_frame_cnt: u32,
    pub flags: u32,
    pub num_tile_columns: u32,
    pub num_tile_rows: u32,
    pub reserved: u32,
    pub tile_widths: *mut u32,
    pub tile_heights: *mut u32,
    pub obu_payload_array_cnt: u32,
    pub reserved1: u32,
    pub obu_payload_array: *mut SeiPayload,
    pub film_grain_params: *mut c_void,
    pub ltr_mark_frame_idx: u32,
    pub ltr_use_frame_bitmap: u32,
    pub num_temporal_layers: u32,
    pub reserved4: u32,
    pub max_cll: *mut ContentLightLevel,
    pub mastering_display: *mut MasteringDisplayInfo,
    pub reserved2: [u32; 242],
    pub reserved3: [*mut c_void; 59],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) union CodecPicParams {
    pub h264: PicParamsH264,
    pub hevc: PicParamsHevc,
    pub av1: PicParamsAv1,
    pub reserved: [u32; 256],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PicParams {
    pub version: u32,
    pub input_width: u32,
    pub input_height: u32,
    pub input_pitch: u32,
    pub encode_pic_flags: u32,
    pub frame_idx: u32,
    pub input_time_stamp: u64,
    pub input_duration: u64,
    pub input_buffer: *mut c_void,
    pub output_bitstream: *mut c_void,
    pub completion_event: *mut c_void,
    pub buffer_fmt: u32,
    pub picture_struct: u32,
    pub picture_type: u32,
    pub codec: CodecPicParams,
    pub me_hint_counts_per_block: [[u32; 4]; 2],
    pub me_external_hints: *mut c_void,
    pub reserved2: [u32; 7],
    pub reserved5: [*mut c_void; 2],
    pub qp_delta_map: *mut i8,
    pub qp_delta_map_size: u32,
    pub reserved_bit_fields: u32,
    pub me_hint_ref_pic_dist: [u16; 2],
    pub reserved4: u32,
    pub alpha_buffer: *mut c_void,
    pub me_external_sb_hints: *mut c_void,
    pub me_sb_hints_count: u32,
    pub state_buffer_idx: u32,
    pub output_recon_buffer: *mut c_void,
    pub reserved3: [u32; 284],
    pub reserved6: [*mut c_void; 57],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct RegisterResource {
    pub version: u32,
    pub resource_type: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub sub_resource_index: u32,
    pub resource_to_register: *mut c_void,
    pub registered_resource: *mut c_void,
    pub buffer_format: u32,
    pub buffer_usage: u32,
    pub input_fence_point: *mut c_void,
    pub chroma_offset: [u32; 2],
    pub chroma_offset_in: [u32; 2],
    pub reserved1: [u32; 244],
    pub reserved2: [*mut c_void; 61],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct MapInputResource {
    pub version: u32,
    pub sub_resource_index: u32,
    pub input_resource: *mut c_void,
    pub registered_resource: *mut c_void,
    pub mapped_resource: *mut c_void,
    pub mapped_buffer_fmt: u32,
    pub reserved1: [u32; 251],
    pub reserved2: [*mut c_void; 63],
}

impl Plain for RcParams {}
impl Plain for Config {}
impl Plain for PresetConfig {}
impl Plain for InitializeParams {}
impl Plain for ReconfigureParams {}
impl Plain for CapsParam {}
impl Plain for OpenEncodeSessionExParams {}
impl Plain for CreateInputBuffer {}
impl Plain for LockInputBuffer {}
impl Plain for CreateBitstreamBuffer {}
impl Plain for LockBitstream {}
impl Plain for PicParams {}
impl Plain for PicParamsH264 {}
impl Plain for PicParamsHevc {}
impl Plain for PicParamsAv1 {}
impl Plain for RegisterResource {}
impl Plain for MapInputResource {}
impl Plain for FunctionList {}
impl Plain for ExternalMemoryHandleDesc {}
impl Plain for ExternalMemoryBufferDesc {}

pub(super) type Status = c_int;
pub(super) type EncoderPtr = *mut c_void;

pub(super) type OpenEncodeSessionExFn =
    unsafe extern "C" fn(*mut OpenEncodeSessionExParams, *mut EncoderPtr) -> Status;
pub(super) type GetEncodeGuidCountFn = unsafe extern "C" fn(EncoderPtr, *mut u32) -> Status;
pub(super) type GetEncodeGuidsFn =
    unsafe extern "C" fn(EncoderPtr, *mut Guid, u32, *mut u32) -> Status;
pub(super) type GetInputFormatCountFn = unsafe extern "C" fn(EncoderPtr, Guid, *mut u32) -> Status;
pub(super) type GetInputFormatsFn =
    unsafe extern "C" fn(EncoderPtr, Guid, *mut u32, u32, *mut u32) -> Status;
pub(super) type GetEncodeCapsFn =
    unsafe extern "C" fn(EncoderPtr, Guid, *mut CapsParam, *mut c_int) -> Status;
pub(super) type GetEncodePresetConfigExFn =
    unsafe extern "C" fn(EncoderPtr, Guid, Guid, u32, *mut PresetConfig) -> Status;
pub(super) type InitializeEncoderFn =
    unsafe extern "C" fn(EncoderPtr, *mut InitializeParams) -> Status;
pub(super) type CreateInputBufferFn =
    unsafe extern "C" fn(EncoderPtr, *mut CreateInputBuffer) -> Status;
pub(super) type DestroyBufferFn = unsafe extern "C" fn(EncoderPtr, *mut c_void) -> Status;
pub(super) type CreateBitstreamBufferFn =
    unsafe extern "C" fn(EncoderPtr, *mut CreateBitstreamBuffer) -> Status;
pub(super) type EncodePictureFn = unsafe extern "C" fn(EncoderPtr, *mut PicParams) -> Status;
pub(super) type LockBitstreamFn = unsafe extern "C" fn(EncoderPtr, *mut LockBitstream) -> Status;
pub(super) type LockInputBufferFn =
    unsafe extern "C" fn(EncoderPtr, *mut LockInputBuffer) -> Status;
pub(super) type MapInputResourceFn =
    unsafe extern "C" fn(EncoderPtr, *mut MapInputResource) -> Status;
pub(super) type DestroyEncoderFn = unsafe extern "C" fn(EncoderPtr) -> Status;
pub(super) type RegisterResourceFn =
    unsafe extern "C" fn(EncoderPtr, *mut RegisterResource) -> Status;
pub(super) type ReconfigureEncoderFn =
    unsafe extern "C" fn(EncoderPtr, *mut ReconfigureParams) -> Status;
pub(super) type GetLastErrorStringFn = unsafe extern "C" fn(EncoderPtr) -> *const c_char;

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct FunctionList {
    pub version: u32,
    pub reserved: u32,
    pub open_encode_session: *mut c_void,
    pub get_encode_guid_count: Option<GetEncodeGuidCountFn>,
    pub get_encode_profile_guid_count: *mut c_void,
    pub get_encode_profile_guids: *mut c_void,
    pub get_encode_guids: Option<GetEncodeGuidsFn>,
    pub get_input_format_count: Option<GetInputFormatCountFn>,
    pub get_input_formats: Option<GetInputFormatsFn>,
    pub get_encode_caps: Option<GetEncodeCapsFn>,
    pub get_encode_preset_count: *mut c_void,
    pub get_encode_preset_guids: *mut c_void,
    pub get_encode_preset_config: *mut c_void,
    pub initialize_encoder: Option<InitializeEncoderFn>,
    pub create_input_buffer: Option<CreateInputBufferFn>,
    pub destroy_input_buffer: Option<DestroyBufferFn>,
    pub create_bitstream_buffer: Option<CreateBitstreamBufferFn>,
    pub destroy_bitstream_buffer: Option<DestroyBufferFn>,
    pub encode_picture: Option<EncodePictureFn>,
    pub lock_bitstream: Option<LockBitstreamFn>,
    pub unlock_bitstream: Option<DestroyBufferFn>,
    pub lock_input_buffer: Option<LockInputBufferFn>,
    pub unlock_input_buffer: Option<DestroyBufferFn>,
    pub get_encode_stats: *mut c_void,
    pub get_sequence_params: *mut c_void,
    pub register_async_event: *mut c_void,
    pub unregister_async_event: *mut c_void,
    pub map_input_resource: Option<MapInputResourceFn>,
    pub unmap_input_resource: Option<DestroyBufferFn>,
    pub destroy_encoder: Option<DestroyEncoderFn>,
    pub invalidate_ref_frames: *mut c_void,
    pub open_encode_session_ex: Option<OpenEncodeSessionExFn>,
    pub register_resource: Option<RegisterResourceFn>,
    pub unregister_resource: Option<DestroyBufferFn>,
    pub reconfigure_encoder: Option<ReconfigureEncoderFn>,
    pub reserved1: *mut c_void,
    pub create_mv_buffer: *mut c_void,
    pub destroy_mv_buffer: *mut c_void,
    pub run_motion_estimation_only: *mut c_void,
    pub get_last_error_string: Option<GetLastErrorStringFn>,
    pub set_io_cuda_streams: *mut c_void,
    pub get_encode_preset_config_ex: Option<GetEncodePresetConfigExFn>,
    pub get_sequence_param_ex: *mut c_void,
    pub restore_encoder_state: *mut c_void,
    pub lookahead_picture: *mut c_void,
    pub reserved2: [*mut c_void; 275],
}

pub(super) type CreateInstanceFn = unsafe extern "C" fn(*mut FunctionList) -> Status;
pub(super) type GetMaxSupportedVersionFn = unsafe extern "C" fn(*mut u32) -> Status;

pub(super) type CuResult = c_int;
pub(super) type CuDevice = c_int;
pub(super) type CuContext = *mut c_void;
pub(super) type CuExternalMemory = *mut c_void;
pub(super) type CuDevicePtr = u64;

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) union ExternalMemoryHandle {
    pub fd: c_int,
    pub win32: [*mut c_void; 2],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ExternalMemoryHandleDesc {
    pub handle_type: u32,
    pub handle: ExternalMemoryHandle,
    pub size: u64,
    pub flags: c_uint,
    pub reserved: [c_uint; 16],
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct ExternalMemoryBufferDesc {
    pub offset: u64,
    pub size: u64,
    pub flags: c_uint,
    pub reserved: [c_uint; 16],
}

pub(super) type CuInitFn = unsafe extern "C" fn(c_uint) -> CuResult;
pub(super) type CuDeviceGetCountFn = unsafe extern "C" fn(*mut c_int) -> CuResult;
pub(super) type CuDeviceGetFn = unsafe extern "C" fn(*mut CuDevice, c_int) -> CuResult;
pub(super) type CuCtxCreateFn = unsafe extern "C" fn(*mut CuContext, c_uint, CuDevice) -> CuResult;
pub(super) type CuCtxDestroyFn = unsafe extern "C" fn(CuContext) -> CuResult;
pub(super) type CuCtxPushCurrentFn = unsafe extern "C" fn(CuContext) -> CuResult;
pub(super) type CuCtxPopCurrentFn = unsafe extern "C" fn(*mut CuContext) -> CuResult;
pub(super) type CuImportExternalMemoryFn =
    unsafe extern "C" fn(*mut CuExternalMemory, *const ExternalMemoryHandleDesc) -> CuResult;
pub(super) type CuExternalMemoryGetMappedBufferFn = unsafe extern "C" fn(
    *mut CuDevicePtr,
    CuExternalMemory,
    *const ExternalMemoryBufferDesc,
) -> CuResult;
pub(super) type CuDestroyExternalMemoryFn = unsafe extern "C" fn(CuExternalMemory) -> CuResult;
pub(super) type CuMemFreeFn = unsafe extern "C" fn(CuDevicePtr) -> CuResult;

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    #[test]
    fn struct_sizes_match_the_sdk_13_0_header() {
        assert_eq!(size_of::<Guid>(), 16);
        assert_eq!(size_of::<FunctionList>(), 2552);
        assert_eq!(size_of::<OpenEncodeSessionExParams>(), 1552);
        assert_eq!(size_of::<CapsParam>(), 256);
        assert_eq!(size_of::<Qp>(), 12);
        assert_eq!(size_of::<RcParams>(), 128);
        assert_eq!(size_of::<VuiParameters>(), 112);
        assert_eq!(size_of::<ConfigH264>(), 1792);
        assert_eq!(size_of::<ConfigHevc>(), 1560);
        assert_eq!(size_of::<ConfigAv1>(), 1552);
        assert_eq!(size_of::<CodecConfig>(), 1792);
        assert_eq!(size_of::<Config>(), 3584);
        assert_eq!(size_of::<PresetConfig>(), 5128);
        assert_eq!(size_of::<InitializeParams>(), 1800);
        assert_eq!(size_of::<ReconfigureParams>(), 1816);
        assert_eq!(size_of::<CreateInputBuffer>(), 776);
        assert_eq!(size_of::<LockInputBuffer>(), 1544);
        assert_eq!(size_of::<CreateBitstreamBuffer>(), 776);
        assert_eq!(size_of::<LockBitstream>(), 1544);
        assert_eq!(size_of::<SeiPayload>(), 16);
        assert_eq!(size_of::<ChromaPoints>(), 4);
        assert_eq!(size_of::<MasteringDisplayInfo>(), 24);
        assert_eq!(size_of::<ContentLightLevel>(), 4);
        assert_eq!(size_of::<PicParamsH264>(), 1536);
        assert_eq!(size_of::<PicParamsHevc>(), 1536);
        assert_eq!(size_of::<PicParamsAv1>(), 1544);
        assert_eq!(size_of::<CodecPicParams>(), 1544);
        assert_eq!(size_of::<PicParams>(), 3360);
        assert_eq!(size_of::<RegisterResource>(), 1536);
        assert_eq!(size_of::<MapInputResource>(), 1544);
    }

    #[test]
    fn cuda_external_memory_descriptors_match_cuda_h() {
        assert_eq!(size_of::<ExternalMemoryHandleDesc>(), 104);
        assert_eq!(size_of::<ExternalMemoryBufferDesc>(), 88);
        assert_eq!(offset_of!(ExternalMemoryHandleDesc, handle), 8);
        assert_eq!(offset_of!(ExternalMemoryHandleDesc, size), 24);
        assert_eq!(offset_of!(ExternalMemoryHandleDesc, flags), 32);
        assert_eq!(offset_of!(ExternalMemoryBufferDesc, flags), 16);
    }

    #[test]
    fn function_list_entries_sit_where_the_driver_writes_them() {
        assert_eq!(offset_of!(FunctionList, open_encode_session_ex), 240);
        assert_eq!(offset_of!(FunctionList, reconfigure_encoder), 264);
        assert_eq!(offset_of!(FunctionList, get_last_error_string), 304);
        assert_eq!(offset_of!(FunctionList, get_encode_preset_config_ex), 320);
        assert_eq!(offset_of!(FunctionList, lookahead_picture), 344);
    }

    #[test]
    fn config_offsets_match_the_sdk_13_0_header() {
        assert_eq!(offset_of!(OpenEncodeSessionExParams, device), 8);
        assert_eq!(offset_of!(OpenEncodeSessionExParams, api_version), 24);
        assert_eq!(offset_of!(RcParams, average_bit_rate), 20);
        assert_eq!(offset_of!(RcParams, vbv_initial_delay), 32);
        assert_eq!(offset_of!(RcParams, min_qp), 40);
        assert_eq!(offset_of!(RcParams, lookahead_depth), 90);
        assert_eq!(offset_of!(RcParams, multi_pass), 100);
        assert_eq!(offset_of!(RcParams, lookahead_level), 112);
        assert_eq!(offset_of!(RcParams, view_bitrate_ratios), 116);
        assert_eq!(offset_of!(RcParams, reserved3), 123);
        assert_eq!(offset_of!(RcParams, reserved1), 124);
        assert_eq!(offset_of!(ConfigH264, level), 4);
        assert_eq!(offset_of!(ConfigH264, intra_refresh_period), 52);
        assert_eq!(offset_of!(ConfigH264, vui), 72);
        assert_eq!(offset_of!(ConfigH264, chroma_format_idc), 192);
        assert_eq!(offset_of!(ConfigH264, input_bit_depth), 216);
        assert_eq!(offset_of!(ConfigH264, tf_level), 220);
        assert_eq!(offset_of!(ConfigH264, reserved1), 224);
        assert_eq!(offset_of!(ConfigH264, reserved2), 1280);
        assert_eq!(offset_of!(ConfigHevc, idr_period), 20);
        assert_eq!(offset_of!(ConfigHevc, vui), 64);
        assert_eq!(offset_of!(ConfigHevc, ltr_trust_mode), 176);
        assert_eq!(offset_of!(ConfigHevc, input_bit_depth), 204);
        assert_eq!(offset_of!(ConfigHevc, num_temporal_layers), 208);
        assert_eq!(offset_of!(ConfigHevc, num_views), 212);
        assert_eq!(offset_of!(ConfigHevc, reserved1), 216);
        assert_eq!(offset_of!(ConfigHevc, reserved2), 1048);
        assert_eq!(offset_of!(ConfigAv1, idr_period), 20);
        assert_eq!(offset_of!(ConfigAv1, tile_widths), 48);
        assert_eq!(offset_of!(ConfigAv1, color_primaries), 68);
        assert_eq!(offset_of!(ConfigAv1, film_grain_params), 96);
        assert_eq!(offset_of!(ConfigAv1, input_bit_depth), 116);
        assert_eq!(offset_of!(ConfigAv1, ltr_num_frames), 120);
        assert_eq!(offset_of!(ConfigAv1, num_temporal_layers), 124);
        assert_eq!(offset_of!(ConfigAv1, tf_level), 128);
        assert_eq!(offset_of!(ConfigAv1, reserved1), 132);
        assert_eq!(offset_of!(ConfigAv1, reserved3), 1056);
        assert_eq!(offset_of!(Config, profile_guid), 4);
        assert_eq!(offset_of!(Config, rc_params), 40);
        assert_eq!(offset_of!(Config, codec), 168);
        assert_eq!(offset_of!(PresetConfig, preset_cfg), 8);
        assert_eq!(offset_of!(InitializeParams, encode_width), 36);
        assert_eq!(offset_of!(InitializeParams, priv_data), 80);
        assert_eq!(offset_of!(InitializeParams, encode_config), 88);
        assert_eq!(
            offset_of!(InitializeParams, max_me_hint_counts_per_block),
            104
        );
        assert_eq!(offset_of!(InitializeParams, tuning_info), 136);
        assert_eq!(offset_of!(InitializeParams, output_stats_level), 148);
        assert_eq!(offset_of!(ReconfigureParams, re_init_encode_params), 8);
        assert_eq!(offset_of!(CreateInputBuffer, input_buffer), 24);
        assert_eq!(offset_of!(LockInputBuffer, buffer_data_ptr), 16);
        assert_eq!(offset_of!(LockInputBuffer, pitch), 24);
        assert_eq!(offset_of!(CreateBitstreamBuffer, bitstream_buffer), 16);
        assert_eq!(offset_of!(LockBitstream, output_bitstream), 8);
        assert_eq!(offset_of!(LockBitstream, bitstream_size_in_bytes), 36);
        assert_eq!(offset_of!(LockBitstream, bitstream_buffer_ptr), 56);
        assert_eq!(offset_of!(LockBitstream, picture_type), 64);
        assert_eq!(offset_of!(LockBitstream, output_stats_ptr), 120);
        assert_eq!(offset_of!(LockBitstream, reserved_internal), 1512);
    }

    #[test]
    fn picture_and_resource_offsets_match_the_sdk_13_0_header() {
        assert_eq!(offset_of!(PicParamsH264, slice_type_data), 24);
        assert_eq!(offset_of!(PicParamsH264, sei_payload_array_cnt), 36);
        assert_eq!(offset_of!(PicParamsH264, sei_payload_array), 40);
        assert_eq!(offset_of!(PicParamsH264, h264_ext_pic_params), 80);
        assert_eq!(offset_of!(PicParamsH264, time_code), 208);
        assert_eq!(offset_of!(PicParamsHevc, slice_type_data), 24);
        assert_eq!(offset_of!(PicParamsHevc, sei_payload_array_cnt), 56);
        assert_eq!(offset_of!(PicParamsHevc, sei_payload_array), 64);
        assert_eq!(offset_of!(PicParamsHevc, time_code), 72);
        assert_eq!(offset_of!(PicParamsHevc, num_temporal_layers), 104);
        assert_eq!(offset_of!(PicParamsHevc, view_id), 108);
        assert_eq!(offset_of!(PicParamsHevc, reference_display_info_3d), 112);
        assert_eq!(offset_of!(PicParamsHevc, max_cll), 120);
        assert_eq!(offset_of!(PicParamsHevc, mastering_display), 128);
        assert_eq!(offset_of!(PicParamsHevc, reserved2), 136);
        assert_eq!(offset_of!(PicParamsHevc, reserved3), 1072);
        assert_eq!(offset_of!(PicParamsAv1, tile_widths), 32);
        assert_eq!(offset_of!(PicParamsAv1, obu_payload_array_cnt), 48);
        assert_eq!(offset_of!(PicParamsAv1, obu_payload_array), 56);
        assert_eq!(offset_of!(PicParamsAv1, film_grain_params), 64);
        assert_eq!(offset_of!(PicParamsAv1, ltr_mark_frame_idx), 72);
        assert_eq!(offset_of!(PicParamsAv1, num_temporal_layers), 80);
        assert_eq!(offset_of!(PicParamsAv1, reserved4), 84);
        assert_eq!(offset_of!(PicParamsAv1, max_cll), 88);
        assert_eq!(offset_of!(PicParamsAv1, mastering_display), 96);
        assert_eq!(offset_of!(PicParamsAv1, reserved2), 104);
        assert_eq!(offset_of!(PicParamsAv1, reserved3), 1072);
        assert_eq!(offset_of!(PicParams, input_time_stamp), 24);
        assert_eq!(offset_of!(PicParams, input_buffer), 40);
        assert_eq!(offset_of!(PicParams, buffer_fmt), 64);
        assert_eq!(offset_of!(PicParams, codec), 80);
        assert_eq!(offset_of!(PicParams, me_hint_counts_per_block), 1624);
        assert_eq!(offset_of!(PicParams, qp_delta_map), 1712);
        assert_eq!(offset_of!(PicParams, me_hint_ref_pic_dist), 1728);
        assert_eq!(offset_of!(PicParams, alpha_buffer), 1736);
        assert_eq!(offset_of!(PicParams, output_recon_buffer), 1760);
        assert_eq!(offset_of!(RegisterResource, resource_to_register), 24);
        assert_eq!(offset_of!(RegisterResource, buffer_format), 40);
        assert_eq!(offset_of!(RegisterResource, input_fence_point), 48);
        assert_eq!(offset_of!(RegisterResource, chroma_offset), 56);
        assert_eq!(offset_of!(RegisterResource, chroma_offset_in), 64);
        assert_eq!(offset_of!(RegisterResource, reserved1), 72);
        assert_eq!(offset_of!(RegisterResource, reserved2), 1048);
        assert_eq!(offset_of!(MasteringDisplayInfo, b), 4);
        assert_eq!(offset_of!(MasteringDisplayInfo, r), 8);
        assert_eq!(offset_of!(MasteringDisplayInfo, white_point), 12);
        assert_eq!(offset_of!(MasteringDisplayInfo, max_luma), 16);
        assert_eq!(offset_of!(MasteringDisplayInfo, min_luma), 20);
        assert_eq!(
            offset_of!(ContentLightLevel, max_pic_average_light_level),
            2
        );
        assert_eq!(offset_of!(MapInputResource, input_resource), 8);
        assert_eq!(offset_of!(MapInputResource, mapped_resource), 24);
        assert_eq!(offset_of!(MapInputResource, mapped_buffer_fmt), 32);
    }

    #[test]
    fn struct_versions_match_the_sdk_13_0_macros() {
        assert_eq!(API_VERSION, 0x0000_000d);
        assert_eq!(MAX_SUPPORTED_VERSION, 0xd0);
        assert_eq!(FUNCTION_LIST_VER, 0x7002_000d);
        assert_eq!(OPEN_ENCODE_SESSION_EX_PARAMS_VER, 0x7001_000d);
        assert_eq!(CAPS_PARAM_VER, 0x7001_000d);
        assert_eq!(RC_PARAMS_VER, 0x7001_000d);
        assert_eq!(CONFIG_VER, 0xf009_000d);
        assert_eq!(PRESET_CONFIG_VER, 0xf005_000d);
        assert_eq!(INITIALIZE_PARAMS_VER, 0xf007_000d);
        assert_eq!(RECONFIGURE_PARAMS_VER, 0xf002_000d);
        assert_eq!(CREATE_INPUT_BUFFER_VER, 0x7002_000d);
        assert_eq!(LOCK_INPUT_BUFFER_VER, 0x7001_000d);
        assert_eq!(CREATE_BITSTREAM_BUFFER_VER, 0x7001_000d);
        assert_eq!(LOCK_BITSTREAM_VER, 0xf002_000d);
        assert_eq!(PIC_PARAMS_VER, 0xf007_000d);
        assert_eq!(REGISTER_RESOURCE_VER, 0x7005_000d);
        assert_eq!(MAP_INPUT_RESOURCE_VER, 0x7004_000d);
    }

    #[test]
    fn caps_queries_are_their_position_in_the_sdk_13_0_enum() {
        assert_eq!(CAPS_SUPPORTED_RATECONTROL_MODES, 1);
        assert_eq!(CAPS_WIDTH_MAX, 16);
        assert_eq!(CAPS_HEIGHT_MAX, 17);
        assert_eq!(CAPS_SUPPORT_INTRA_REFRESH, 25);
        assert_eq!(CAPS_SUPPORT_YUV444_ENCODE, 33);
        assert_eq!(CAPS_SUPPORT_TEMPORAL_AQ, 38);
        assert_eq!(CAPS_SUPPORT_10BIT_ENCODE, 39);
        assert_eq!(CAPS_WIDTH_MIN, 45);
        assert_eq!(CAPS_HEIGHT_MIN, 46);
        assert_eq!(CAPS_SUPPORT_YUV422_ENCODE, 59);
    }

    #[test]
    fn sdk_13_0_buffer_formats_and_profiles_match_the_header() {
        assert_eq!(BUFFER_FORMAT_NV16, 0x4000_0001);
        assert_eq!(BUFFER_FORMAT_P210, 0x4000_0002);
        assert_eq!(H264_PROFILE_HIGH_10_GUID.data1, 0x8f0c_337e);
        assert_eq!(H264_PROFILE_HIGH_422_GUID.data4[7], 0x33);
        assert_eq!(SPLIT_FOUR_FORCED_MODE, 4);
    }

    #[test]
    fn sdk_13_0_hdr_flags_sit_after_the_mv_hevc_and_svc_bits() {
        let mut hevc = 0;
        hevc_bits::OUTPUT_MAX_CLL.set(&mut hevc, 1);
        hevc_bits::OUTPUT_MASTERING_DISPLAY.set(&mut hevc, 1);
        let mut av1 = 0;
        av1_bits::OUTPUT_MAX_CLL.set(&mut av1, 1);
        av1_bits::OUTPUT_MASTERING_DISPLAY.set(&mut av1, 1);

        assert_eq!(hevc, 0b11 << 23);
        assert_eq!(av1, 0b11 << 14);
    }

    #[test]
    fn bit_fields_set_only_their_own_bits() {
        let mut word = u32::MAX;
        hevc_bits::CHROMA_FORMAT_IDC.set(&mut word, CHROMA_FORMAT_IDC_420);

        assert_eq!(hevc_bits::CHROMA_FORMAT_IDC.get(word), 1);
        assert_eq!(word, !(0b10 << 9));

        let mut word = 0;
        rc_bits::AQ_STRENGTH.set(&mut word, 0xff);
        av1_bits::CHROMA_FORMAT_IDC.set(&mut word, CHROMA_FORMAT_IDC_444);

        assert_eq!(word, (0xf << 12) | (0b11 << 7));
    }
}
