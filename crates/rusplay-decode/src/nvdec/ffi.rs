use std::{
    ffi::{c_int, c_longlong, c_short, c_uchar, c_uint, c_ulong, c_ulonglong, c_ushort, c_void},
    ptr,
};

pub(super) type CuResult = c_int;
pub(super) type CuDevice = c_int;
pub(super) type CuContext = *mut c_void;
pub(super) type CuDevicePtr = c_ulonglong;
pub(super) type CuVideoDecoder = *mut c_void;
pub(super) type CuVideoParser = *mut c_void;
pub(super) type CuVideoTimestamp = c_longlong;

pub(super) const CUDA_SUCCESS: CuResult = 0;

pub(super) const CUDA_VIDEO_CODEC_H264: c_int = 4;
pub(super) const CUDA_VIDEO_CODEC_HEVC: c_int = 8;
pub(super) const CUDA_VIDEO_CODEC_AV1: c_int = 11;

#[cfg(test)]
pub(super) const CHROMA_FORMAT_MONOCHROME: c_int = 0;
pub(super) const CHROMA_FORMAT_420: c_int = 1;
pub(super) const CHROMA_FORMAT_422: c_int = 2;
pub(super) const CHROMA_FORMAT_444: c_int = 3;

pub(super) const SURFACE_FORMAT_NV12: c_int = 0;
pub(super) const SURFACE_FORMAT_P016: c_int = 1;
pub(super) const SURFACE_FORMAT_YUV444: c_int = 2;
pub(super) const SURFACE_FORMAT_YUV444_16BIT: c_int = 3;
pub(super) const SURFACE_FORMAT_NV16: c_int = 4;
pub(super) const SURFACE_FORMAT_P216: c_int = 5;

pub(super) const DEINTERLACE_MODE_WEAVE: c_int = 0;
pub(super) const CREATE_PREFER_CUVID: c_ulong = 0x04;

pub(super) const PACKET_END_OF_STREAM: c_ulong = 0x01;
pub(super) const PACKET_TIMESTAMP: c_ulong = 0x02;
pub(super) const PACKET_END_OF_PICTURE: c_ulong = 0x08;

pub(super) const MEMORY_TYPE_HOST: c_int = 1;
pub(super) const MEMORY_TYPE_DEVICE: c_int = 2;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ShortRect {
    pub left: c_short,
    pub top: c_short,
    pub right: c_short,
    pub bottom: c_short,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct IntRect {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DecodeCaps {
    pub codec: c_int,
    pub chroma_format: c_int,
    pub bit_depth_minus8: c_uint,
    pub reserved1: [c_uint; 3],
    pub is_supported: c_uchar,
    pub num_nvdecs: c_uchar,
    pub output_format_mask: c_ushort,
    pub max_width: c_uint,
    pub max_height: c_uint,
    pub max_mb_count: c_uint,
    pub min_width: c_ushort,
    pub min_height: c_ushort,
    pub histogram_supported: c_uchar,
    pub counter_bit_depth: c_uchar,
    pub max_histogram_bins: c_ushort,
    pub decode_stats_supported: c_uchar,
    pub reserved4: [c_uchar; 3],
    pub reserved3: [c_uint; 9],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct DecodeCreateInfo {
    pub width: c_ulong,
    pub height: c_ulong,
    pub num_decode_surfaces: c_ulong,
    pub codec: c_int,
    pub chroma_format: c_int,
    pub creation_flags: c_ulong,
    pub bit_depth_minus8: c_ulong,
    pub intra_decode_only: c_ulong,
    pub max_width: c_ulong,
    pub max_height: c_ulong,
    pub reserved1: c_ulong,
    pub display_area: ShortRect,
    pub output_format: c_int,
    pub deinterlace_mode: c_int,
    pub target_width: c_ulong,
    pub target_height: c_ulong,
    pub num_output_surfaces: c_ulong,
    pub vid_lock: *mut c_void,
    pub target_rect: ShortRect,
    pub enable_histogram: c_ulong,
    pub enable_decode_features: c_ulong,
    pub reserved2: [c_ulong; 3],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct FrameRate {
    pub numerator: c_uint,
    pub denominator: c_uint,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct VideoFormat {
    pub codec: c_int,
    pub frame_rate: FrameRate,
    pub progressive_sequence: c_uchar,
    pub bit_depth_luma_minus8: c_uchar,
    pub bit_depth_chroma_minus8: c_uchar,
    pub min_num_decode_surfaces: c_uchar,
    pub coded_width: c_uint,
    pub coded_height: c_uint,
    pub display_area: IntRect,
    pub chroma_format: c_int,
    pub bitrate: c_uint,
    pub display_aspect_ratio: [c_int; 2],
    pub video_signal_description: [c_uchar; 4],
    pub seqhdr_data_length: c_uint,
}

#[repr(C)]
pub(super) struct PicParams {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ParserDisplayInfo {
    pub picture_index: c_int,
    pub progressive_frame: c_int,
    pub top_field_first: c_int,
    pub repeat_first_field: c_int,
    pub timestamp: CuVideoTimestamp,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct SourceDataPacket {
    pub flags: c_ulong,
    pub payload_size: c_ulong,
    pub payload: *const c_uchar,
    pub timestamp: CuVideoTimestamp,
}

pub(super) type SequenceCallback = unsafe extern "C" fn(*mut c_void, *mut VideoFormat) -> c_int;
pub(super) type DecodeCallback = unsafe extern "C" fn(*mut c_void, *mut PicParams) -> c_int;
pub(super) type DisplayCallback =
    unsafe extern "C" fn(*mut c_void, *mut ParserDisplayInfo) -> c_int;
pub(super) type OpaqueCallback = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;

#[repr(C)]
#[derive(Debug)]
pub(super) struct ParserParams {
    pub codec: c_int,
    pub max_num_decode_surfaces: c_uint,
    pub clock_rate: c_uint,
    pub error_threshold: c_uint,
    pub max_display_delay: c_uint,
    pub flags: c_uint,
    pub reserved1: [c_uint; 4],
    pub user_data: *mut c_void,
    pub sequence_callback: Option<SequenceCallback>,
    pub decode_picture: Option<DecodeCallback>,
    pub display_picture: Option<DisplayCallback>,
    pub get_operating_point: Option<OpaqueCallback>,
    pub get_sei_message: Option<OpaqueCallback>,
    pub reserved2: [*mut c_void; 5],
    pub ext_video_info: *mut c_void,
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct ProcParams {
    pub progressive_frame: c_int,
    pub second_field: c_int,
    pub top_field_first: c_int,
    pub unpaired_field: c_int,
    pub reserved_flags: c_uint,
    pub reserved_zero: c_uint,
    pub raw_input_dptr: c_ulonglong,
    pub raw_input_pitch: c_uint,
    pub raw_input_format: c_uint,
    pub raw_output_dptr: c_ulonglong,
    pub raw_output_pitch: c_uint,
    pub reserved1: c_uint,
    pub output_stream: *mut c_void,
    pub reserved: [c_uint; 46],
    pub histogram_dptr: *mut c_ulonglong,
    pub proc_ext: *mut c_void,
}

impl ProcParams {
    pub(super) fn for_display(info: &ParserDisplayInfo) -> Self {
        Self {
            progressive_frame: info.progressive_frame,
            second_field: 0,
            top_field_first: info.top_field_first,
            unpaired_field: c_int::from(info.repeat_first_field < 0),
            reserved_flags: 0,
            reserved_zero: 0,
            raw_input_dptr: 0,
            raw_input_pitch: 0,
            raw_input_format: 0,
            raw_output_dptr: 0,
            raw_output_pitch: 0,
            reserved1: 0,
            output_stream: ptr::null_mut(),
            reserved: [0; 46],
            histogram_dptr: ptr::null_mut(),
            proc_ext: ptr::null_mut(),
        }
    }
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct Memcpy2D {
    pub src_x_in_bytes: usize,
    pub src_y: usize,
    pub src_memory_type: c_int,
    pub src_host: *const c_void,
    pub src_device: CuDevicePtr,
    pub src_array: *mut c_void,
    pub src_pitch: usize,
    pub dst_x_in_bytes: usize,
    pub dst_y: usize,
    pub dst_memory_type: c_int,
    pub dst_host: *mut c_void,
    pub dst_device: CuDevicePtr,
    pub dst_array: *mut c_void,
    pub dst_pitch: usize,
    pub width_in_bytes: usize,
    pub height: usize,
}

impl Memcpy2D {
    pub(super) fn device_to_host(
        source: CuDevicePtr,
        source_pitch: usize,
        destination: &mut [u8],
        destination_pitch: usize,
        width_in_bytes: usize,
        height: usize,
    ) -> Self {
        Self {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: MEMORY_TYPE_DEVICE,
            src_host: ptr::null(),
            src_device: source,
            src_array: ptr::null_mut(),
            src_pitch: source_pitch,
            dst_x_in_bytes: 0,
            dst_y: 0,
            dst_memory_type: MEMORY_TYPE_HOST,
            dst_host: destination.as_mut_ptr().cast(),
            dst_device: 0,
            dst_array: ptr::null_mut(),
            dst_pitch: destination_pitch,
            width_in_bytes,
            height,
        }
    }
}

pub(super) type CuInit = unsafe extern "C" fn(c_uint) -> CuResult;
pub(super) type CuDeviceGetCount = unsafe extern "C" fn(*mut c_int) -> CuResult;
pub(super) type CuDeviceGet = unsafe extern "C" fn(*mut CuDevice, c_int) -> CuResult;
pub(super) type CuDevicePrimaryCtxRetain =
    unsafe extern "C" fn(*mut CuContext, CuDevice) -> CuResult;
pub(super) type CuDevicePrimaryCtxRelease = unsafe extern "C" fn(CuDevice) -> CuResult;
pub(super) type CuCtxPushCurrent = unsafe extern "C" fn(CuContext) -> CuResult;
pub(super) type CuCtxPopCurrent = unsafe extern "C" fn(*mut CuContext) -> CuResult;
pub(super) type CuMemcpy2D = unsafe extern "C" fn(*const Memcpy2D) -> CuResult;

pub(super) type CuvidGetDecoderCaps = unsafe extern "C" fn(*mut DecodeCaps) -> CuResult;
pub(super) type CuvidCreateDecoder =
    unsafe extern "C" fn(*mut CuVideoDecoder, *mut DecodeCreateInfo) -> CuResult;
pub(super) type CuvidDestroyDecoder = unsafe extern "C" fn(CuVideoDecoder) -> CuResult;
pub(super) type CuvidDecodePicture =
    unsafe extern "C" fn(CuVideoDecoder, *mut PicParams) -> CuResult;
pub(super) type CuvidMapVideoFrame = unsafe extern "C" fn(
    CuVideoDecoder,
    c_int,
    *mut CuDevicePtr,
    *mut c_uint,
    *mut ProcParams,
) -> CuResult;
pub(super) type CuvidUnmapVideoFrame =
    unsafe extern "C" fn(CuVideoDecoder, CuDevicePtr) -> CuResult;
pub(super) type CuvidCreateVideoParser =
    unsafe extern "C" fn(*mut CuVideoParser, *mut ParserParams) -> CuResult;
pub(super) type CuvidParseVideoData =
    unsafe extern "C" fn(CuVideoParser, *mut SourceDataPacket) -> CuResult;
pub(super) type CuvidDestroyVideoParser = unsafe extern "C" fn(CuVideoParser) -> CuResult;

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    #[test]
    fn decode_caps_matches_cuviddec() {
        assert_eq!(size_of::<DecodeCaps>(), 88);
        assert_eq!(offset_of!(DecodeCaps, is_supported), 24);
        assert_eq!(offset_of!(DecodeCaps, output_format_mask), 26);
        assert_eq!(offset_of!(DecodeCaps, max_mb_count), 36);
        assert_eq!(offset_of!(DecodeCaps, min_width), 40);
    }

    #[test]
    fn decode_create_info_matches_cuviddec() {
        assert_eq!(size_of::<DecodeCreateInfo>(), 176);
        assert_eq!(offset_of!(DecodeCreateInfo, codec), 24);
        assert_eq!(offset_of!(DecodeCreateInfo, creation_flags), 32);
        assert_eq!(offset_of!(DecodeCreateInfo, display_area), 80);
        assert_eq!(offset_of!(DecodeCreateInfo, output_format), 88);
        assert_eq!(offset_of!(DecodeCreateInfo, target_width), 96);
        assert_eq!(offset_of!(DecodeCreateInfo, vid_lock), 120);
        assert_eq!(offset_of!(DecodeCreateInfo, target_rect), 128);
        assert_eq!(offset_of!(DecodeCreateInfo, enable_histogram), 136);
    }

    #[test]
    fn video_format_matches_nvcuvid() {
        assert_eq!(size_of::<VideoFormat>(), 64);
        assert_eq!(offset_of!(VideoFormat, progressive_sequence), 12);
        assert_eq!(offset_of!(VideoFormat, min_num_decode_surfaces), 15);
        assert_eq!(offset_of!(VideoFormat, coded_width), 16);
        assert_eq!(offset_of!(VideoFormat, display_area), 24);
        assert_eq!(offset_of!(VideoFormat, chroma_format), 40);
        assert_eq!(offset_of!(VideoFormat, video_signal_description), 56);
        assert_eq!(offset_of!(VideoFormat, seqhdr_data_length), 60);
    }

    #[test]
    fn parser_structs_match_nvcuvid() {
        assert_eq!(size_of::<ParserDisplayInfo>(), 24);
        assert_eq!(offset_of!(ParserDisplayInfo, timestamp), 16);
        assert_eq!(size_of::<SourceDataPacket>(), 32);
        assert_eq!(size_of::<ParserParams>(), 136);
        assert_eq!(offset_of!(ParserParams, flags), 20);
        assert_eq!(offset_of!(ParserParams, user_data), 40);
        assert_eq!(offset_of!(ParserParams, sequence_callback), 48);
        assert_eq!(offset_of!(ParserParams, get_sei_message), 80);
        assert_eq!(offset_of!(ParserParams, ext_video_info), 128);
    }

    #[test]
    fn proc_params_match_cuviddec() {
        assert_eq!(size_of::<ProcParams>(), 264);
        assert_eq!(offset_of!(ProcParams, raw_input_dptr), 24);
        assert_eq!(offset_of!(ProcParams, output_stream), 56);
        assert_eq!(offset_of!(ProcParams, histogram_dptr), 248);
        assert_eq!(offset_of!(ProcParams, proc_ext), 256);
    }

    #[test]
    fn memcpy_2d_matches_cuda() {
        assert_eq!(size_of::<Memcpy2D>(), 128);
        assert_eq!(offset_of!(Memcpy2D, src_host), 24);
        assert_eq!(offset_of!(Memcpy2D, src_pitch), 48);
        assert_eq!(offset_of!(Memcpy2D, dst_x_in_bytes), 56);
        assert_eq!(offset_of!(Memcpy2D, dst_host), 80);
        assert_eq!(offset_of!(Memcpy2D, width_in_bytes), 112);
    }

    #[test]
    fn display_info_round_trips_through_proc_params() {
        let unpaired = ParserDisplayInfo {
            picture_index: 3,
            progressive_frame: 0,
            top_field_first: 1,
            repeat_first_field: -1,
            timestamp: 0,
        };

        let params = ProcParams::for_display(&unpaired);

        assert_eq!(params.progressive_frame, 0);
        assert_eq!(params.top_field_first, 1);
        assert_eq!(params.unpaired_field, 1);
    }
}
