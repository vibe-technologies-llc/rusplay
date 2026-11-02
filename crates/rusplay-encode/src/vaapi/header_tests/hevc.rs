use std::{io::Cursor, rc::Rc};

use cros_codecs::codec::h265::parser::{
    Nalu, NaluType, Parser, Pps, ProfileTierLevel, SliceHeader, SliceType, Sps, Vps,
};
use rusplay_core::{BitDepth, ChromaSubsampling, FrameRate, PixelFormat, Resolution};

use super::{
    assert_no_start_code_emulation, color_cases, every_format, frame_rates, picture, pixel,
    stream_info,
};
use crate::vaapi::{
    Picture, ffi,
    hevc::{HevcProfile, HevcStream},
};

struct Geometry {
    resolution: Resolution,
    coded_width: u32,
    coded_height: u32,
    crop_right: u32,
    crop_bottom: u32,
}

const fn geometry(
    width: u32,
    height: u32,
    coded_width: u32,
    coded_height: u32,
    crop_right: u32,
    crop_bottom: u32,
) -> Geometry {
    Geometry {
        resolution: Resolution::new(width, height),
        coded_width,
        coded_height,
        crop_right,
        crop_bottom,
    }
}

const GEOMETRIES: [Geometry; 5] = [
    geometry(402, 226, 408, 232, 6, 6),
    geometry(1366, 768, 1368, 768, 2, 0),
    geometry(1920, 1080, 1920, 1080, 0, 0),
    geometry(400, 216, 400, 216, 0, 0),
    geometry(8, 8, 8, 8, 0, 0),
];

struct ProfileCase {
    format: PixelFormat,
    idc: u8,
    compatible: &'static [usize],
    range_extension: Option<[bool; 5]>,
}

fn profile_cases() -> Vec<ProfileCase> {
    let case = |chroma, depth, idc, compatible, range_extension| ProfileCase {
        format: pixel(chroma, depth),
        idc,
        compatible,
        range_extension,
    };
    let (c420, c422, c444) = (
        ChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444,
    );
    let (d8, d10, d12) = (BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve);
    let stripped: &'static [usize] = &[4];

    vec![
        case(c420, d8, 1, &[1, 2], None),
        case(c420, d10, 2, &[2], None),
        case(
            c420,
            d12,
            4,
            stripped,
            Some([true, false, false, true, true]),
        ),
        case(
            c422,
            d8,
            4,
            stripped,
            Some([true, true, false, true, false]),
        ),
        case(
            c422,
            d10,
            4,
            stripped,
            Some([true, true, false, true, false]),
        ),
        case(
            c422,
            d12,
            4,
            stripped,
            Some([true, false, false, true, false]),
        ),
        case(
            c444,
            d8,
            4,
            stripped,
            Some([true, true, true, false, false]),
        ),
        case(
            c444,
            d10,
            4,
            stripped,
            Some([true, true, false, false, false]),
        ),
        case(
            c444,
            d12,
            4,
            stripped,
            Some([true, false, false, false, false]),
        ),
    ]
}

struct Parsed {
    vps: Rc<Vps>,
    sps: Rc<Sps>,
    pps: Rc<Pps>,
    nal_types: [NaluType; 3],
    nal_ids: [(u8, u8); 3],
}

fn stream(format: PixelFormat, resolution: Resolution) -> HevcStream {
    HevcStream::new(
        HevcProfile::for_format(format),
        stream_info(format, resolution),
    )
}

fn parse(stream: &HevcStream) -> Parsed {
    let mut data = stream.vps();
    data.extend(stream.sps());
    data.extend(stream.pps());
    let mut parser = Parser::default();
    let mut cursor = Cursor::new(data.as_slice());

    let vps_nalu = Nalu::next(&mut cursor).unwrap();
    parser.parse_vps(&vps_nalu).unwrap();
    let sps_nalu = Nalu::next(&mut cursor).unwrap();
    parser.parse_sps(&sps_nalu).unwrap();
    let pps_nalu = Nalu::next(&mut cursor).unwrap();
    parser.parse_pps(&pps_nalu).unwrap();
    let nalus = [&vps_nalu, &sps_nalu, &pps_nalu];

    Parsed {
        vps: Rc::clone(parser.get_vps(0).unwrap()),
        sps: Rc::clone(parser.get_sps(0).unwrap()),
        pps: Rc::clone(parser.get_pps(0).unwrap()),
        nal_types: nalus.map(|nalu| nalu.header.type_),
        nal_ids: nalus.map(|nalu| (nalu.header.nuh_layer_id, nalu.header.nuh_temporal_id_plus1)),
    }
}

struct ParsedSlice {
    header: SliceHeader,
    nal_type: NaluType,
    layer_id: u8,
    temporal_id_plus1: u8,
}

fn parse_slice(stream: &HevcStream, keyframe: bool, order: i32) -> ParsedSlice {
    let mut data = stream.vps();
    data.extend(stream.sps());
    data.extend(stream.pps());
    data.extend(HevcStream::slice_header(keyframe, order).bytes);
    let mut parser = Parser::default();
    let mut cursor = Cursor::new(data.as_slice());

    let vps = Nalu::next(&mut cursor).unwrap();
    parser.parse_vps(&vps).unwrap();
    let sps = Nalu::next(&mut cursor).unwrap();
    parser.parse_sps(&sps).unwrap();
    let pps = Nalu::next(&mut cursor).unwrap();
    parser.parse_pps(&pps).unwrap();
    let slice_nalu = Nalu::next(&mut cursor).unwrap();
    let nal_type = slice_nalu.header.type_;
    let layer_id = slice_nalu.header.nuh_layer_id;
    let temporal_id_plus1 = slice_nalu.header.nuh_temporal_id_plus1;
    let slice = parser.parse_slice_header(slice_nalu).unwrap();

    ParsedSlice {
        header: slice.header,
        nal_type,
        layer_id,
        temporal_id_plus1,
    }
}

fn assert_profile_tier_level(
    ptl: &ProfileTierLevel,
    case: &ProfileCase,
    level: u8,
    high_tier: bool,
) {
    assert_eq!(ptl.general_profile_space, 0);
    assert_eq!(ptl.general_tier_flag, high_tier);
    assert_eq!(ptl.general_profile_idc, case.idc);
    for (index, flag) in ptl.general_profile_compatibility_flag.iter().enumerate() {
        assert_eq!(
            *flag,
            case.compatible.contains(&index),
            "compatibility flag {index}"
        );
    }
    assert!(ptl.general_progressive_source_flag);
    assert!(!ptl.general_interlaced_source_flag);
    assert!(!ptl.general_non_packed_constraint_flag);
    assert!(ptl.general_frame_only_constraint_flag);
    if let Some([max_12, max_10, max_8, max_422, max_420]) = case.range_extension {
        assert_eq!(ptl.general_max_12bit_constraint_flag, max_12);
        assert_eq!(ptl.general_max_10bit_constraint_flag, max_10);
        assert_eq!(ptl.general_max_8bit_constraint_flag, max_8);
        assert_eq!(ptl.general_max_422chroma_constraint_flag, max_422);
        assert_eq!(ptl.general_max_420chroma_constraint_flag, max_420);
        assert!(!ptl.general_max_monochrome_constraint_flag);
        assert!(!ptl.general_intra_constraint_flag);
        assert!(!ptl.general_one_picture_only_constraint_flag);
        assert!(ptl.general_lower_bit_rate_constraint_flag);
    }
    assert!(!ptl.general_inbld_flag);
    assert_eq!(ptl.general_level_idc as u8, level);
    assert!(ptl.sub_layer_profile_present_flag.iter().all(|flag| !flag));
    assert!(ptl.sub_layer_level_present_flag.iter().all(|flag| !flag));
}

#[test]
fn vps_carries_every_field_the_writer_sets() {
    for format in every_format() {
        for frame_rate in frame_rates() {
            let mut info = stream_info(format, Resolution::new(1920, 1080));
            info.frame_rate = frame_rate;
            let parsed = parse(&HevcStream::new(HevcProfile::for_format(format), info));
            let vps = &parsed.vps;

            assert_eq!(parsed.nal_types[0], NaluType::VpsNut);
            assert_eq!(vps.video_parameter_set_id, 0);
            assert!(vps.base_layer_internal_flag);
            assert!(vps.base_layer_available_flag);
            assert_eq!(vps.max_layers_minus1, 0);
            assert_eq!(vps.max_sub_layers_minus1, 0);
            assert!(vps.temporal_id_nesting_flag);
            assert_eq!(vps.profile_tier_level, parsed.sps.profile_tier_level);
            assert!(!vps.sub_layer_ordering_info_present_flag);
            assert_eq!(vps.max_dec_pic_buffering_minus1[0], 1);
            assert_eq!(vps.max_num_reorder_pics[0], 0);
            assert_eq!(vps.max_latency_increase_plus1[0], 0);
            assert_eq!(vps.max_layer_id, 0);
            assert_eq!(vps.num_layer_sets_minus1, 0);
            assert!(vps.timing_info_present_flag);
            assert_eq!(vps.num_units_in_tick, frame_rate.denominator);
            assert_eq!(vps.time_scale, frame_rate.numerator);
            assert!(!vps.poc_proportional_to_timing_flag);
            assert_eq!(vps.num_hrd_parameters, 0);
            assert!(!vps.extension_flag);
        }
    }
}

#[test]
fn profile_tier_level_matches_the_format() {
    for case in profile_cases() {
        let stream = stream(case.format, Resolution::new(1920, 1080));
        let parsed = parse(&stream);

        assert_profile_tier_level(&parsed.sps.profile_tier_level, &case, 123, false);
        assert_profile_tier_level(&parsed.vps.profile_tier_level, &case, 123, false);
    }
}

#[test]
fn level_and_tier_follow_resolution_rate_and_bitrate() {
    let cases = [
        (1920, 1080, 60, 20_000, 123, false),
        (1920, 1080, 60, 25_000, 123, true),
        (400, 216, 30, 2_000, 63, false),
        (400, 216, 30, 1_000, 60, false),
        (3840, 2160, 60, 40_000, 153, false),
        (3840, 2160, 60, 45_000, 153, true),
        (1280, 720, 30, 5_000, 93, false),
    ];

    for (width, height, fps, kbps, level, high_tier) in cases {
        let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
        let mut info = stream_info(format, Resolution::new(width, height));
        info.frame_rate = FrameRate::integer(fps);
        info.peak_kbps = kbps;
        let stream = HevcStream::new(HevcProfile::Main, info);

        let parsed = parse(&stream);
        let parameters = stream.sequence_parameters(1_000);
        let ptl = &parsed.sps.profile_tier_level;

        assert_eq!(
            ptl.general_level_idc as u8, level,
            "{width}x{height}@{fps} {kbps}"
        );
        assert_eq!(
            ptl.general_tier_flag, high_tier,
            "{width}x{height}@{fps} {kbps}"
        );
        assert_eq!(parameters.general_level_idc, level);
        assert_eq!(parameters.general_tier_flag == 1, high_tier);
    }
}

#[test]
fn sps_carries_every_field_the_writer_sets() {
    for case in profile_cases() {
        for geometry in &GEOMETRIES {
            let stream = stream(case.format, geometry.resolution);
            let parsed = parse(&stream);
            let sps = &parsed.sps;
            let rect = sps.visible_rectangle();
            let (unit_x, unit_y) = match case.format.chroma {
                ChromaSubsampling::Yuv420 => (2, 2),
                ChromaSubsampling::Yuv422 => (2, 1),
                ChromaSubsampling::Yuv444 => (1, 1),
            };
            let chroma_format_idc = match case.format.chroma {
                ChromaSubsampling::Yuv420 => 1,
                ChromaSubsampling::Yuv422 => 2,
                ChromaSubsampling::Yuv444 => 3,
            };

            assert_eq!(parsed.nal_types[1], NaluType::SpsNut);
            assert_eq!(sps.video_parameter_set_id, 0);
            assert_eq!(sps.max_sub_layers_minus1, 0);
            assert!(sps.temporal_id_nesting_flag);
            assert_eq!(sps.seq_parameter_set_id, 0);
            assert_eq!(sps.chroma_format_idc, chroma_format_idc);
            assert!(!sps.separate_colour_plane_flag);
            assert_eq!(
                u32::from(sps.pic_width_in_luma_samples),
                geometry.coded_width
            );
            assert_eq!(
                u32::from(sps.pic_height_in_luma_samples),
                geometry.coded_height
            );
            assert_eq!(
                sps.conformance_window_flag,
                geometry.crop_right != 0 || geometry.crop_bottom != 0
            );
            assert_eq!(sps.conf_win_left_offset, 0);
            assert_eq!(sps.conf_win_right_offset * unit_x, geometry.crop_right);
            assert_eq!(sps.conf_win_top_offset, 0);
            assert_eq!(sps.conf_win_bottom_offset * unit_y, geometry.crop_bottom);
            assert_eq!(rect.min.x, 0);
            assert_eq!(rect.min.y, 0);
            assert_eq!(rect.max.x, geometry.resolution.width);
            assert_eq!(rect.max.y, geometry.resolution.height);
            assert_eq!(sps.bit_depth_luma_minus8, case.format.depth.bits() - 8);
            assert_eq!(sps.bit_depth_chroma_minus8, case.format.depth.bits() - 8);
            assert_eq!(sps.log2_max_pic_order_cnt_lsb_minus4, 12);
            assert!(sps.sub_layer_ordering_info_present_flag);
            assert_eq!(sps.max_dec_pic_buffering_minus1[0], 1);
            assert_eq!(sps.max_num_reorder_pics[0], 0);
            assert_eq!(sps.max_latency_increase_plus1[0], 0);
            assert_eq!(sps.log2_min_luma_coding_block_size_minus3, 0);
            assert_eq!(sps.log2_diff_max_min_luma_coding_block_size, 3);
            assert_eq!(sps.log2_min_luma_transform_block_size_minus2, 0);
            assert_eq!(sps.log2_diff_max_min_luma_transform_block_size, 3);
            assert_eq!(sps.max_transform_hierarchy_depth_inter, 2);
            assert_eq!(sps.max_transform_hierarchy_depth_intra, 2);
            assert!(!sps.scaling_list_enabled_flag);
            assert!(sps.amp_enabled_flag);
            assert!(sps.sample_adaptive_offset_enabled_flag);
            assert!(!sps.pcm_enabled_flag);
            assert_eq!(sps.num_short_term_ref_pic_sets, 1);
            let reference_set = &sps.short_term_ref_pic_set[0];
            assert!(!reference_set.inter_ref_pic_set_prediction_flag);
            assert_eq!(reference_set.num_negative_pics, 1);
            assert_eq!(reference_set.num_positive_pics, 0);
            assert_eq!(reference_set.delta_poc_s0[0], -1);
            assert!(reference_set.used_by_curr_pic_s0[0]);
            assert!(!sps.long_term_ref_pics_present_flag);
            assert!(!sps.temporal_mvp_enabled_flag);
            assert!(!sps.strong_intra_smoothing_enabled_flag);
            assert!(sps.vui_parameters_present_flag);
        }
    }
}

#[test]
fn vui_signals_colour_timing_and_restrictions() {
    for case in color_cases() {
        for frame_rate in frame_rates() {
            for format in every_format() {
                let mut info = stream_info(format, Resolution::new(1920, 1080));
                info.color = case.color;
                info.frame_rate = frame_rate;
                let parsed = parse(&HevcStream::new(HevcProfile::for_format(format), info));
                let vui = &parsed.sps.vui_parameters;

                assert!(vui.aspect_ratio_info_present_flag);
                assert_eq!(vui.aspect_ratio_idc, 1);
                assert!(!vui.overscan_info_present_flag);
                assert!(vui.video_signal_type_present_flag);
                assert_eq!(vui.video_format, 5);
                assert_eq!(vui.video_full_range_flag, case.full_range);
                assert!(vui.colour_description_present_flag);
                assert_eq!(vui.colour_primaries, u32::from(case.primaries));
                assert_eq!(vui.transfer_characteristics, u32::from(case.transfer));
                assert_eq!(vui.matrix_coeffs, u32::from(case.matrix));
                assert!(vui.chroma_loc_info_present_flag);
                assert_eq!(vui.chroma_sample_loc_type_top_field, 0);
                assert_eq!(vui.chroma_sample_loc_type_bottom_field, 0);
                assert!(!vui.neutral_chroma_indication_flag);
                assert!(!vui.field_seq_flag);
                assert!(!vui.frame_field_info_present_flag);
                assert!(!vui.default_display_window_flag);
                assert!(vui.timing_info_present_flag);
                assert_eq!(vui.num_units_in_tick, frame_rate.denominator);
                assert_eq!(vui.time_scale, frame_rate.numerator);
                assert!(!vui.poc_proportional_to_timing_flag);
                assert!(!vui.hrd_parameters_present_flag);
                assert!(vui.bitstream_restriction_flag);
                assert!(!vui.tiles_fixed_structure_flag);
                assert!(vui.motion_vectors_over_pic_boundaries_flag);
                assert!(vui.restricted_ref_pic_lists_flag);
                assert_eq!(vui.min_spatial_segmentation_idc, 0);
                assert_eq!(vui.max_bytes_per_pic_denom, 0);
                assert_eq!(vui.max_bits_per_min_cu_denom, 0);
                assert_eq!(vui.log2_max_mv_length_horizontal, 15);
                assert_eq!(vui.log2_max_mv_length_vertical, 15);
            }
        }
    }
}

#[test]
fn pps_carries_every_field_the_writer_sets() {
    let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);

    for (qp, delta) in [(None, 0), (Some(18), -8), (Some(26), 0), (Some(40), 14)] {
        let mut info = stream_info(format, Resolution::new(402, 226));
        info.qp = qp;
        let parsed = parse(&HevcStream::new(HevcProfile::Main, info));
        let pps = &parsed.pps;

        assert_eq!(parsed.nal_types[2], NaluType::PpsNut);
        assert_eq!(pps.pic_parameter_set_id, 0);
        assert_eq!(pps.seq_parameter_set_id, 0);
        assert!(!pps.dependent_slice_segments_enabled_flag);
        assert!(!pps.output_flag_present_flag);
        assert_eq!(pps.num_extra_slice_header_bits, 0);
        assert!(!pps.sign_data_hiding_enabled_flag);
        assert!(!pps.cabac_init_present_flag);
        assert_eq!(pps.num_ref_idx_l0_default_active_minus1, 0);
        assert_eq!(pps.num_ref_idx_l1_default_active_minus1, 0);
        assert_eq!(pps.init_qp_minus26, delta);
        assert!(!pps.constrained_intra_pred_flag);
        assert!(!pps.transform_skip_enabled_flag);
        assert!(pps.cu_qp_delta_enabled_flag);
        assert_eq!(pps.diff_cu_qp_delta_depth, 0);
        assert_eq!(pps.cb_qp_offset, 0);
        assert_eq!(pps.cr_qp_offset, 0);
        assert!(!pps.slice_chroma_qp_offsets_present_flag);
        assert!(!pps.weighted_pred_flag);
        assert!(!pps.weighted_bipred_flag);
        assert!(!pps.transquant_bypass_enabled_flag);
        assert!(!pps.tiles_enabled_flag);
        assert!(!pps.entropy_coding_sync_enabled_flag);
        assert!(pps.loop_filter_across_slices_enabled_flag);
        assert!(!pps.deblocking_filter_control_present_flag);
        assert!(!pps.scaling_list_data_present_flag);
        assert!(!pps.lists_modification_present_flag);
        assert_eq!(pps.log2_parallel_merge_level_minus2, 0);
    }
}

#[test]
fn nal_units_are_layer_zero_temporal_zero_and_escaped() {
    for format in every_format() {
        let stream = stream(format, Resolution::new(402, 226));
        let parsed = parse(&stream);

        assert_eq!(parsed.nal_ids, [(0, 1); 3]);
        for bytes in [stream.vps(), stream.sps(), stream.pps()] {
            assert_no_start_code_emulation(&bytes);
            assert_ne!(*bytes.last().unwrap(), 0);
        }
        assert_eq!(
            stream.sequence_header().bytes,
            [stream.vps(), stream.sps()].concat()
        );
        assert_eq!(
            stream.sequence_header().bits,
            stream.sequence_header().bytes.len() * 8
        );
    }
}

#[test]
fn slice_headers_carry_type_poc_and_sao() {
    let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
    let stream = stream(format, Resolution::new(402, 226));
    let orders = [
        (0, 0),
        (1, 1),
        (65_535, 65_535),
        (65_536, 0),
        (65_537, 1),
        (131_075, 3),
        (i32::MAX, 65_535),
    ];

    for (order, lsb) in orders {
        let inter = parse_slice(&stream, false, order);
        let header = &inter.header;

        assert_eq!(inter.nal_type, NaluType::TrailR);
        assert_eq!((inter.layer_id, inter.temporal_id_plus1), (0, 1));
        assert!(header.first_slice_segment_in_pic_flag);
        assert_eq!(header.pic_parameter_set_id, 0);
        assert_eq!(header.type_, SliceType::P);
        assert_eq!(header.pic_order_cnt_lsb, lsb, "order {order}");
        assert!(header.short_term_ref_pic_set_sps_flag);
        assert!(header.sao_luma_flag);
        assert!(header.sao_chroma_flag);
        assert!(!header.num_ref_idx_active_override_flag);
        assert_eq!(header.five_minus_max_num_merge_cand, 0);
        assert_eq!(header.qp_delta, 0);
        assert!(header.loop_filter_across_slices_enabled_flag);
        assert_no_start_code_emulation(&HevcStream::slice_header(false, order).bytes);
    }

    let key = parse_slice(&stream, true, 0);

    assert_eq!(key.nal_type, NaluType::IdrWRadl);
    assert_eq!((key.layer_id, key.temporal_id_plus1), (0, 1));
    assert!(key.header.first_slice_segment_in_pic_flag);
    assert!(!key.header.no_output_of_prior_pics_flag);
    assert_eq!(key.header.pic_parameter_set_id, 0);
    assert_eq!(key.header.type_, SliceType::I);
    assert!(key.header.sao_luma_flag);
    assert!(key.header.sao_chroma_flag);
    assert_eq!(key.header.qp_delta, 0);
    assert!(key.header.loop_filter_across_slices_enabled_flag);
}

#[test]
fn packed_slice_header_covers_whole_bytes() {
    for keyframe in [true, false] {
        let packed = HevcStream::slice_header(keyframe, 3);

        assert!(packed.bits <= packed.bytes.len() * 8);
        assert!(packed.bytes.len() * 8 - packed.bits < 8);
    }
}

#[test]
fn sequence_parameters_agree_with_the_sps() {
    for format in every_format() {
        for geometry in &GEOMETRIES {
            let stream = stream(format, geometry.resolution);
            let parsed = parse(&stream);
            let sps = &parsed.sps;
            let ptl = &sps.profile_tier_level;

            let parameters = stream.sequence_parameters(9_876_543);
            let sequence = unsafe { parameters.seq_fields.bits };

            assert_eq!(parameters.general_profile_idc, ptl.general_profile_idc);
            assert_eq!(parameters.general_level_idc, ptl.general_level_idc as u8);
            assert_eq!(parameters.general_tier_flag == 1, ptl.general_tier_flag);
            assert_eq!(parameters.intra_period, 0xffff);
            assert_eq!(parameters.intra_idr_period, 0xffff);
            assert_eq!(parameters.ip_period, 1);
            assert_eq!(parameters.bits_per_second, 9_876_543);
            assert_eq!(
                parameters.pic_width_in_luma_samples,
                sps.pic_width_in_luma_samples
            );
            assert_eq!(
                parameters.pic_height_in_luma_samples,
                sps.pic_height_in_luma_samples
            );
            assert_eq!(
                parameters.log2_min_luma_coding_block_size_minus3,
                sps.log2_min_luma_coding_block_size_minus3
            );
            assert_eq!(
                parameters.log2_diff_max_min_luma_coding_block_size,
                sps.log2_diff_max_min_luma_coding_block_size
            );
            assert_eq!(
                parameters.log2_min_transform_block_size_minus2,
                sps.log2_min_luma_transform_block_size_minus2
            );
            assert_eq!(
                parameters.log2_diff_max_min_transform_block_size,
                sps.log2_diff_max_min_luma_transform_block_size
            );
            assert_eq!(
                parameters.max_transform_hierarchy_depth_inter,
                sps.max_transform_hierarchy_depth_inter
            );
            assert_eq!(
                parameters.max_transform_hierarchy_depth_intra,
                sps.max_transform_hierarchy_depth_intra
            );
            assert_eq!(
                sequence.chroma_format_idc(),
                u32::from(sps.chroma_format_idc)
            );
            assert_eq!(
                sequence.bit_depth_luma_minus8(),
                u32::from(sps.bit_depth_luma_minus8)
            );
            assert_eq!(
                sequence.bit_depth_chroma_minus8(),
                u32::from(sps.bit_depth_chroma_minus8)
            );
            assert_eq!(sequence.amp_enabled_flag() == 1, sps.amp_enabled_flag);
            assert_eq!(
                sequence.sample_adaptive_offset_enabled_flag() == 1,
                sps.sample_adaptive_offset_enabled_flag
            );
            assert_eq!(sequence.pcm_enabled_flag() == 1, sps.pcm_enabled_flag);
            assert_eq!(
                sequence.scaling_list_enabled_flag() == 1,
                sps.scaling_list_enabled_flag
            );
            assert_eq!(
                sequence.sps_temporal_mvp_enabled_flag() == 1,
                sps.temporal_mvp_enabled_flag
            );
            assert_eq!(
                sequence.strong_intra_smoothing_enabled_flag() == 1,
                sps.strong_intra_smoothing_enabled_flag
            );
            assert_eq!(sequence.low_delay_seq(), 1);
        }
    }
}

#[test]
fn vui_parameters_agree_with_the_vui() {
    for format in every_format() {
        for frame_rate in frame_rates() {
            let mut info = stream_info(format, Resolution::new(1920, 1080));
            info.frame_rate = frame_rate;
            let stream = HevcStream::new(HevcProfile::for_format(format), info);
            let parsed = parse(&stream);
            let sps = &parsed.sps;
            let vui = &sps.vui_parameters;

            let parameters = stream.sequence_parameters(9_876_543);
            let fields = unsafe { parameters.vui_fields.bits };

            assert_eq!(
                parameters.vui_parameters_present_flag == 1,
                sps.vui_parameters_present_flag
            );
            assert_eq!(u32::from(parameters.aspect_ratio_idc), vui.aspect_ratio_idc);
            assert_eq!(parameters.vui_num_units_in_tick, vui.num_units_in_tick);
            assert_eq!(parameters.vui_time_scale, vui.time_scale);
            assert_eq!(
                fields.aspect_ratio_info_present_flag() == 1,
                vui.aspect_ratio_info_present_flag
            );
            assert_eq!(
                fields.vui_timing_info_present_flag() == 1,
                vui.timing_info_present_flag
            );
            assert_eq!(
                fields.bitstream_restriction_flag() == 1,
                vui.bitstream_restriction_flag
            );
            assert_eq!(
                fields.motion_vectors_over_pic_boundaries_flag() == 1,
                vui.motion_vectors_over_pic_boundaries_flag
            );
            assert_eq!(
                fields.restricted_ref_pic_lists_flag() == 1,
                vui.restricted_ref_pic_lists_flag
            );
            assert_eq!(
                fields.log2_max_mv_length_horizontal(),
                vui.log2_max_mv_length_horizontal
            );
            assert_eq!(
                fields.log2_max_mv_length_vertical(),
                vui.log2_max_mv_length_vertical
            );
        }
    }
}

#[test]
fn picture_and_slice_parameters_follow_the_stream_state() {
    let format = pixel(ChromaSubsampling::Yuv420, BitDepth::Eight);
    let gop = [(0, true), (1, false), (2, false), (0, true), (1, false)];

    for geometry in &GEOMETRIES {
        let mut stream = stream(format, geometry.resolution);
        let parsed = parse(&stream);
        let mut previous: Option<Picture> = None;

        for (position, (index, keyframe)) in gop.into_iter().enumerate() {
            let picture = Picture {
                recon: 200 + u32::try_from(position).unwrap(),
                ..picture(index, keyframe)
            };
            stream.begin(&picture);
            let order = HevcStream::order(&picture);
            let slice = parse_slice(&stream, keyframe, order);

            let picture_parameters = stream.picture_parameters(&picture, order);
            let slice_parameters = stream.slice_parameters(&picture);
            let fields = unsafe { picture_parameters.pic_fields.bits };
            let slice_fields = unsafe { slice_parameters.slice_fields.bits };
            let ctus = geometry.coded_width.div_ceil(64) * geometry.coded_height.div_ceil(64);

            assert_eq!(order, i32::try_from(index).unwrap());
            assert_eq!(
                picture_parameters.decoded_curr_pic.picture_id,
                picture.recon
            );
            assert_eq!(picture_parameters.decoded_curr_pic.pic_order_cnt, order);
            assert_eq!(picture_parameters.coded_buf, picture.coded);
            assert_eq!(
                i32::from(picture_parameters.pic_init_qp) - 26,
                i32::from(parsed.pps.init_qp_minus26)
            );
            assert_eq!(fields.idr_pic_flag() == 1, keyframe);
            assert_eq!(fields.coding_type(), if keyframe { 1 } else { 2 });
            assert_eq!(fields.reference_pic_flag(), 1);
            assert_eq!(
                fields.cu_qp_delta_enabled_flag() == 1,
                parsed.pps.cu_qp_delta_enabled_flag
            );
            assert_eq!(
                fields.pps_loop_filter_across_slices_enabled_flag() == 1,
                parsed.pps.loop_filter_across_slices_enabled_flag
            );
            assert_eq!(
                picture_parameters.nal_unit_type,
                if keyframe { 19 } else { 1 }
            );
            assert_eq!(slice.nal_type as u8, picture_parameters.nal_unit_type);
            assert_eq!(slice_parameters.slice_type, slice.header.type_ as u8);
            assert_eq!(slice_parameters.num_ctu_in_slice, ctus);
            assert_eq!(
                slice_parameters.max_num_merge_cand,
                5 - slice.header.five_minus_max_num_merge_cand
            );
            assert_eq!(slice_fields.last_slice_of_pic_flag(), 1);
            assert_eq!(
                slice_fields.slice_sao_luma_flag() == 1,
                slice.header.sao_luma_flag
            );
            assert_eq!(
                slice_fields.slice_sao_chroma_flag() == 1,
                slice.header.sao_chroma_flag
            );
            assert_eq!(
                slice_fields.slice_loop_filter_across_slices_enabled_flag() == 1,
                slice.header.loop_filter_across_slices_enabled_flag
            );
            if keyframe {
                assert_eq!(
                    picture_parameters.reference_frames[0].flags,
                    ffi::VA_PICTURE_HEVC_INVALID
                );
                assert_eq!(
                    slice_parameters.ref_pic_list0[0].flags,
                    ffi::VA_PICTURE_HEVC_INVALID
                );
            } else {
                let previous = previous.unwrap();
                let reference = picture_parameters.reference_frames[0];

                assert_eq!(reference.picture_id, previous.recon);
                assert_eq!(reference.pic_order_cnt, order - 1);
                assert_eq!(reference.flags, ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE);
                assert_eq!(slice_parameters.ref_pic_list0[0].picture_id, previous.recon);
                assert_eq!(slice_parameters.ref_pic_list0[0].pic_order_cnt, order - 1);
            }
            assert_eq!(
                picture_parameters.reference_frames[1].flags,
                ffi::VA_PICTURE_HEVC_INVALID
            );

            stream.commit(&picture, order);
            previous = Some(picture);
        }
    }
}
