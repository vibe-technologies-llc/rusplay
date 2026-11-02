use std::{io::Cursor, rc::Rc};

use cros_codecs::codec::h264::parser::{Nalu, NaluType, Parser, Pps, SliceHeader, SliceType, Sps};
use rusplay_core::{BitDepth, ChromaSubsampling, FrameRate, Resolution};

use super::{
    assert_no_start_code_emulation, color_cases, frame_rates, picture, pixel, stream_info,
};
use crate::vaapi::{
    Picture, ffi,
    h264::{H264Profile, H264Stream},
};

const PROFILES: [H264Profile; 3] = [
    H264Profile::ConstrainedBaseline,
    H264Profile::Main,
    H264Profile::High,
];

struct Geometry {
    resolution: Resolution,
    width_mbs: u32,
    height_mbs: u32,
    crop_right: u32,
    crop_bottom: u32,
}

const fn geometry(
    width: u32,
    height: u32,
    width_mbs: u32,
    height_mbs: u32,
    crop_right: u32,
    crop_bottom: u32,
) -> Geometry {
    Geometry {
        resolution: Resolution::new(width, height),
        width_mbs,
        height_mbs,
        crop_right,
        crop_bottom,
    }
}

const GEOMETRIES: [Geometry; 5] = [
    geometry(402, 226, 26, 15, 7, 7),
    geometry(1920, 1080, 120, 68, 0, 4),
    geometry(1280, 720, 80, 45, 0, 0),
    geometry(400, 208, 25, 13, 0, 0),
    geometry(16, 16, 1, 1, 0, 0),
];

struct Parsed {
    sps: Rc<Sps>,
    pps: Rc<Pps>,
    sps_ref_idc: u8,
    pps_ref_idc: u8,
}

fn yuv420() -> rusplay_core::PixelFormat {
    pixel(ChromaSubsampling::Yuv420, BitDepth::Eight)
}

fn stream(profile: H264Profile, resolution: Resolution) -> H264Stream {
    H264Stream::new(profile, stream_info(yuv420(), resolution))
}

fn parse(stream: &H264Stream) -> Parsed {
    let mut data = stream.sps().bytes;
    data.extend(stream.pps().bytes);
    let mut parser = Parser::default();
    let mut cursor = Cursor::new(data.as_slice());

    let sps_nalu = Nalu::next(&mut cursor).unwrap();
    let pps_nalu = Nalu::next(&mut cursor).unwrap();
    assert_eq!(sps_nalu.header.type_, NaluType::Sps);
    assert_eq!(pps_nalu.header.type_, NaluType::Pps);
    parser.parse_sps(&sps_nalu).unwrap();
    parser.parse_pps(&pps_nalu).unwrap();

    Parsed {
        sps: Rc::clone(parser.get_sps(0).unwrap()),
        pps: Rc::clone(parser.get_pps(0).unwrap()),
        sps_ref_idc: sps_nalu.header.ref_idc,
        pps_ref_idc: pps_nalu.header.ref_idc,
    }
}

struct ParsedSlice {
    header: SliceHeader,
    nal_type: NaluType,
    ref_idc: u8,
    packed_bits: usize,
    packed_bytes: usize,
}

fn parse_slice(stream: &H264Stream, keyframe: bool) -> ParsedSlice {
    let mut data = stream.sps().bytes;
    data.extend(stream.pps().bytes);
    let packed = stream.slice_header(keyframe);
    data.extend(&packed.bytes);
    let mut parser = Parser::default();
    let mut cursor = Cursor::new(data.as_slice());

    let sps_nalu = Nalu::next(&mut cursor).unwrap();
    parser.parse_sps(&sps_nalu).unwrap();
    let pps_nalu = Nalu::next(&mut cursor).unwrap();
    parser.parse_pps(&pps_nalu).unwrap();
    let slice_nalu = Nalu::next(&mut cursor).unwrap();
    let nal_type = slice_nalu.header.type_;
    let ref_idc = slice_nalu.header.ref_idc;
    let slice = parser.parse_slice_header(slice_nalu).unwrap();

    ParsedSlice {
        header: slice.header,
        nal_type,
        ref_idc,
        packed_bits: packed.bits,
        packed_bytes: packed.bytes.len(),
    }
}

fn level_of(sps: &Sps) -> u8 {
    sps.level_idc as u8
}

#[test]
fn sps_carries_every_field_the_writer_sets() {
    for profile in PROFILES {
        for geometry in &GEOMETRIES {
            let stream = stream(profile, geometry.resolution);
            let parsed = parse(&stream);
            let sps = &parsed.sps;
            let rect = sps.visible_rectangle();
            let (idc, constraints) = match profile {
                H264Profile::ConstrainedBaseline => (66, [true, true, false]),
                H264Profile::Main => (77, [false, true, false]),
                H264Profile::High => (100, [false, false, false]),
            };

            assert_eq!(parsed.sps_ref_idc, 3);
            assert_eq!(sps.seq_parameter_set_id, 0);
            assert_eq!(sps.profile_idc, idc);
            assert_eq!(
                [
                    sps.constraint_set0_flag,
                    sps.constraint_set1_flag,
                    sps.constraint_set2_flag
                ],
                constraints
            );
            assert!(!sps.constraint_set3_flag);
            assert!(!sps.constraint_set4_flag);
            assert!(!sps.constraint_set5_flag);
            assert_eq!(sps.chroma_format_idc, 1);
            assert!(!sps.separate_colour_plane_flag);
            assert_eq!(sps.bit_depth_luma_minus8, 0);
            assert_eq!(sps.bit_depth_chroma_minus8, 0);
            assert!(!sps.qpprime_y_zero_transform_bypass_flag);
            assert!(!sps.seq_scaling_matrix_present_flag);
            assert_eq!(sps.log2_max_frame_num_minus4, 12);
            assert_eq!(sps.pic_order_cnt_type, 2);
            assert_eq!(sps.max_num_ref_frames, 1);
            assert!(!sps.gaps_in_frame_num_value_allowed_flag);
            assert_eq!(
                u32::from(sps.pic_width_in_mbs_minus1) + 1,
                geometry.width_mbs
            );
            assert_eq!(
                u32::from(sps.pic_height_in_map_units_minus1) + 1,
                geometry.height_mbs
            );
            assert!(sps.frame_mbs_only_flag);
            assert!(!sps.mb_adaptive_frame_field_flag);
            assert!(sps.direct_8x8_inference_flag);
            assert_eq!(
                sps.frame_cropping_flag,
                geometry.crop_right != 0 || geometry.crop_bottom != 0
            );
            assert_eq!(sps.frame_crop_left_offset, 0);
            assert_eq!(sps.frame_crop_right_offset, geometry.crop_right);
            assert_eq!(sps.frame_crop_top_offset, 0);
            assert_eq!(sps.frame_crop_bottom_offset, geometry.crop_bottom);
            assert_eq!(rect.min.x, 0);
            assert_eq!(rect.min.y, 0);
            assert_eq!(rect.max.x, geometry.resolution.width);
            assert_eq!(rect.max.y, geometry.resolution.height);
            assert!(sps.vui_parameters_present_flag);
        }
    }
}

#[test]
fn vui_signals_colour_timing_and_restrictions() {
    for case in color_cases() {
        for frame_rate in frame_rates() {
            for profile in PROFILES {
                let mut info = stream_info(yuv420(), Resolution::new(1920, 1080));
                info.color = case.color;
                info.frame_rate = frame_rate;
                let parsed = parse(&H264Stream::new(profile, info));
                let vui = &parsed.sps.vui_parameters;

                assert!(vui.aspect_ratio_info_present_flag);
                assert_eq!(vui.aspect_ratio_idc, 1);
                assert!(!vui.overscan_info_present_flag);
                assert!(vui.video_signal_type_present_flag);
                assert_eq!(vui.video_format, 5);
                assert_eq!(vui.video_full_range_flag, case.full_range);
                assert!(vui.colour_description_present_flag);
                assert_eq!(vui.colour_primaries, case.primaries);
                assert_eq!(vui.transfer_characteristics, case.transfer);
                assert_eq!(vui.matrix_coefficients, case.matrix);
                assert!(vui.chroma_loc_info_present_flag);
                assert_eq!(vui.chroma_sample_loc_type_top_field, 0);
                assert_eq!(vui.chroma_sample_loc_type_bottom_field, 0);
                assert!(vui.timing_info_present_flag);
                assert_eq!(vui.num_units_in_tick, frame_rate.denominator);
                assert_eq!(vui.time_scale, frame_rate.numerator * 2);
                assert!(!vui.fixed_frame_rate_flag);
                assert!(!vui.nal_hrd_parameters_present_flag);
                assert!(!vui.vcl_hrd_parameters_present_flag);
                assert!(!vui.pic_struct_present_flag);
                assert!(vui.bitstream_restriction_flag);
                assert!(vui.motion_vectors_over_pic_boundaries_flag);
                assert_eq!(vui.max_bytes_per_pic_denom, 0);
                assert_eq!(vui.max_bits_per_mb_denom, 0);
                assert_eq!(vui.log2_max_mv_length_horizontal, 15);
                assert_eq!(vui.log2_max_mv_length_vertical, 15);
                assert_eq!(vui.max_num_reorder_frames, 0);
                assert_eq!(vui.max_dec_frame_buffering, 1);
            }
        }
    }
}

#[test]
fn level_follows_resolution_rate_bitrate_and_profile() {
    let cases = [
        (H264Profile::High, 1920, 1080, 60, 20_000, 42),
        (H264Profile::Main, 1920, 1080, 60, 20_000, 42),
        (H264Profile::High, 400, 216, 30, 5_000, 21),
        (H264Profile::High, 400, 216, 30, 2_500, 20),
        (H264Profile::Main, 400, 216, 30, 2_500, 21),
        (H264Profile::ConstrainedBaseline, 400, 216, 30, 2_500, 21),
        (H264Profile::High, 400, 216, 30, 900, 13),
        (H264Profile::High, 1280, 720, 30, 10_000, 31),
    ];

    for (profile, width, height, fps, kbps, level) in cases {
        let mut info = stream_info(yuv420(), Resolution::new(width, height));
        info.frame_rate = FrameRate::integer(fps);
        info.peak_kbps = kbps;
        let stream = H264Stream::new(profile, info);

        let parsed = parse(&stream);
        let parameters = stream.sequence_parameters(1_000);

        assert_eq!(
            level_of(&parsed.sps),
            level,
            "{profile:?} {width}x{height}@{fps} {kbps} kbps"
        );
        assert_eq!(parameters.level_idc, level);
    }
}

#[test]
fn pps_carries_profile_tools_and_the_initial_qp() {
    for profile in PROFILES {
        for (qp, delta) in [(None, 0), (Some(18), -8), (Some(26), 0), (Some(40), 14)] {
            let mut info = stream_info(yuv420(), Resolution::new(402, 226));
            info.qp = qp;
            let parsed = parse(&H264Stream::new(profile, info));
            let pps = &parsed.pps;

            assert_eq!(parsed.pps_ref_idc, 3);
            assert_eq!(pps.pic_parameter_set_id, 0);
            assert_eq!(pps.seq_parameter_set_id, 0);
            assert_eq!(
                pps.entropy_coding_mode_flag,
                profile != H264Profile::ConstrainedBaseline
            );
            assert!(!pps.bottom_field_pic_order_in_frame_present_flag);
            assert_eq!(pps.num_slice_groups_minus1, 0);
            assert_eq!(pps.num_ref_idx_l0_default_active_minus1, 0);
            assert_eq!(pps.num_ref_idx_l1_default_active_minus1, 0);
            assert!(!pps.weighted_pred_flag);
            assert_eq!(pps.weighted_bipred_idc, 0);
            assert_eq!(pps.pic_init_qp_minus26, delta);
            assert_eq!(pps.pic_init_qs_minus26, 0);
            assert_eq!(pps.chroma_qp_index_offset, 0);
            assert!(pps.deblocking_filter_control_present_flag);
            assert!(!pps.constrained_intra_pred_flag);
            assert!(!pps.redundant_pic_cnt_present_flag);
            assert_eq!(pps.transform_8x8_mode_flag, profile == H264Profile::High);
            assert!(!pps.pic_scaling_matrix_present_flag);
            assert_eq!(pps.second_chroma_qp_index_offset, 0);
        }
    }
}

struct Step {
    picture: Picture,
    previous: Option<Picture>,
    order: i32,
    frame_num: usize,
    idr_pic_id: usize,
}

fn drive(profile: H264Profile, mut check: impl FnMut(&H264Stream, &Parsed, &Step)) {
    let gop = [
        (0, true, 0, 0),
        (1, false, 1, 0),
        (2, false, 2, 0),
        (0, true, 0, 1),
        (1, false, 1, 1),
        (2, false, 2, 1),
        (3, false, 3, 1),
        (0, true, 0, 2),
    ];
    let mut stream = stream(profile, Resolution::new(402, 226));
    let parsed = parse(&stream);
    let mut previous: Option<Picture> = None;

    for (position, (index, keyframe, frame_num, idr_pic_id)) in gop.into_iter().enumerate() {
        let picture = Picture {
            recon: 200 + u32::try_from(position).unwrap(),
            ..picture(index, keyframe)
        };
        stream.begin(&picture);
        let order = H264Stream::order(&picture);

        let step = Step {
            picture,
            previous,
            order,
            frame_num,
            idr_pic_id,
        };
        check(&stream, &parsed, &step);

        stream.commit(&picture, order);
        previous = Some(picture);
    }
}

#[test]
fn slice_headers_follow_the_stream_state() {
    for profile in PROFILES {
        drive(profile, |stream, _, step| {
            let keyframe = step.picture.keyframe;
            let slice = parse_slice(stream, keyframe);
            let header = &slice.header;

            assert_eq!(header.first_mb_in_slice, 0);
            assert_eq!(
                header.slice_type,
                if keyframe { SliceType::I } else { SliceType::P }
            );
            assert_eq!(header.pic_parameter_set_id, 0);
            assert_eq!(usize::from(header.frame_num), step.frame_num);
            assert!(!header.field_pic_flag);
            assert!(!header.num_ref_idx_active_override_flag);
            assert!(!header.ref_pic_list_modification_flag_l0);
            assert_eq!(header.slice_qp_delta, 0);
            assert_eq!(header.disable_deblocking_filter_idc, 0);
            assert_eq!(header.slice_alpha_c0_offset_div2, 0);
            assert_eq!(header.slice_beta_offset_div2, 0);
            assert_eq!(header.cabac_init_idc, 0);
            assert_eq!(slice.ref_idc, 3);
            if keyframe {
                assert_eq!(slice.nal_type, NaluType::SliceIdr);
                assert_eq!(usize::from(header.idr_pic_id), step.idr_pic_id);
                assert!(!header.dec_ref_pic_marking.no_output_of_prior_pics_flag);
                assert!(!header.dec_ref_pic_marking.long_term_reference_flag);
            } else {
                assert_eq!(slice.nal_type, NaluType::Slice);
                assert!(
                    !header
                        .dec_ref_pic_marking
                        .adaptive_ref_pic_marking_mode_flag
                );
            }
            assert!(slice.packed_bits <= slice.packed_bytes * 8);
            assert!(slice.packed_bytes * 8 - slice.packed_bits < 8);
        });
    }
}

#[test]
fn va_picture_and_slice_parameters_follow_the_stream_state() {
    for profile in PROFILES {
        drive(profile, |stream, parsed, step| {
            let picture = &step.picture;
            let slice = parse_slice(stream, picture.keyframe);
            let picture_parameters = stream.picture_parameters(picture, step.order);
            let slice_parameters = stream.slice_parameters(picture);
            let fields = unsafe { picture_parameters.pic_fields.bits };
            let current = picture_parameters.CurrPic;
            let reference = picture_parameters.ReferenceFrames[0];
            let listed = slice_parameters.RefPicList0[0];

            assert_eq!(usize::from(picture_parameters.frame_num), step.frame_num);
            assert_eq!(current.frame_idx, u32::from(slice.header.frame_num));
            assert_eq!(current.picture_id, picture.recon);
            assert_eq!(current.TopFieldOrderCnt, step.order);
            assert_eq!(current.BottomFieldOrderCnt, step.order);
            assert_eq!(picture_parameters.coded_buf, picture.coded);
            assert_eq!(
                i32::from(picture_parameters.pic_init_qp) - 26,
                i32::from(parsed.pps.pic_init_qp_minus26)
            );
            assert_eq!(fields.idr_pic_flag() == 1, picture.keyframe);
            assert_eq!(fields.reference_pic_flag(), 1);
            assert_eq!(
                fields.entropy_coding_mode_flag() == 1,
                parsed.pps.entropy_coding_mode_flag
            );
            assert_eq!(
                fields.transform_8x8_mode_flag() == 1,
                parsed.pps.transform_8x8_mode_flag
            );
            assert_eq!(
                fields.deblocking_filter_control_present_flag() == 1,
                parsed.pps.deblocking_filter_control_present_flag
            );
            assert_eq!(slice_parameters.slice_type, slice.header.slice_type as u8);
            assert_eq!(
                slice_parameters.num_macroblocks,
                (u32::from(parsed.sps.pic_width_in_mbs_minus1) + 1)
                    * (u32::from(parsed.sps.pic_height_in_map_units_minus1) + 1)
            );
            assert_eq!(slice_parameters.macroblock_info, ffi::VA_INVALID_ID);
            assert_eq!(
                picture_parameters.ReferenceFrames[1].flags,
                ffi::VA_PICTURE_H264_INVALID
            );

            if picture.keyframe {
                assert_eq!(usize::from(slice_parameters.idr_pic_id), step.idr_pic_id);
                assert_eq!(slice_parameters.idr_pic_id, slice.header.idr_pic_id);
                assert_eq!(reference.flags, ffi::VA_PICTURE_H264_INVALID);
                assert_eq!(listed.flags, ffi::VA_PICTURE_H264_INVALID);
            } else {
                let previous = step.previous.unwrap();

                assert_eq!(reference.picture_id, previous.recon);
                assert_eq!(reference.flags, ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE);
                assert_eq!(
                    usize::try_from(reference.frame_idx).unwrap(),
                    step.frame_num - 1
                );
                assert_eq!(reference.TopFieldOrderCnt, step.order - 2);
                assert_eq!(listed.picture_id, previous.recon);
                assert_eq!(listed.flags, ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE);
            }
        });
    }
}

#[test]
fn frame_num_wraps_at_sixteen_bits() {
    let mut stream = stream(H264Profile::High, Resolution::new(400, 208));
    let key = picture(0, true);
    stream.begin(&key);
    stream.commit(&key, 0);

    for index in 1..65_535_u64 {
        let inter = picture(index, false);
        stream.begin(&inter);
        stream.commit(&inter, 0);
    }

    let before = picture(65_535, false);
    stream.begin(&before);
    let last = parse_slice(&stream, false).header.frame_num;
    stream.commit(&before, 0);

    let after = picture(65_536, false);
    stream.begin(&after);
    let wrapped = parse_slice(&stream, false).header.frame_num;

    assert_eq!(last, 65_535);
    assert_eq!(wrapped, 0);
}

#[test]
fn sequence_parameters_agree_with_the_sps() {
    for profile in PROFILES {
        for geometry in &GEOMETRIES {
            for frame_rate in frame_rates() {
                let mut info = stream_info(yuv420(), geometry.resolution);
                info.frame_rate = frame_rate;
                let stream = H264Stream::new(profile, info);
                let parsed = parse(&stream);
                let sps = &parsed.sps;
                let vui = &sps.vui_parameters;

                let parameters = stream.sequence_parameters(12_345_678);
                let sequence = unsafe { parameters.seq_fields.bits };
                let vui_fields = unsafe { parameters.vui_fields.bits };

                assert_eq!(parameters.level_idc, level_of(sps));
                assert_eq!(parameters.bits_per_second, 12_345_678);
                assert_eq!(
                    parameters.max_num_ref_frames,
                    u32::from(sps.max_num_ref_frames)
                );
                assert_eq!(
                    parameters.picture_width_in_mbs,
                    sps.pic_width_in_mbs_minus1 + 1
                );
                assert_eq!(
                    parameters.picture_height_in_mbs,
                    sps.pic_height_in_map_units_minus1 + 1
                );
                assert_eq!(parameters.intra_period, 0xffff);
                assert_eq!(parameters.intra_idr_period, 0xffff);
                assert_eq!(parameters.ip_period, 1);
                assert_eq!(parameters.frame_cropping_flag == 1, sps.frame_cropping_flag);
                assert_eq!(
                    parameters.frame_crop_left_offset,
                    sps.frame_crop_left_offset
                );
                assert_eq!(
                    parameters.frame_crop_right_offset,
                    sps.frame_crop_right_offset
                );
                assert_eq!(parameters.frame_crop_top_offset, sps.frame_crop_top_offset);
                assert_eq!(
                    parameters.frame_crop_bottom_offset,
                    sps.frame_crop_bottom_offset
                );
                assert_eq!(
                    parameters.vui_parameters_present_flag == 1,
                    sps.vui_parameters_present_flag
                );
                assert_eq!(parameters.aspect_ratio_idc, vui.aspect_ratio_idc);
                assert_eq!(parameters.num_units_in_tick, vui.num_units_in_tick);
                assert_eq!(parameters.time_scale, vui.time_scale);
                assert_eq!(
                    sequence.chroma_format_idc(),
                    u32::from(sps.chroma_format_idc)
                );
                assert_eq!(sequence.frame_mbs_only_flag() == 1, sps.frame_mbs_only_flag);
                assert_eq!(
                    sequence.direct_8x8_inference_flag() == 1,
                    sps.direct_8x8_inference_flag
                );
                assert_eq!(
                    sequence.log2_max_frame_num_minus4(),
                    u32::from(sps.log2_max_frame_num_minus4)
                );
                assert_eq!(
                    sequence.pic_order_cnt_type(),
                    u32::from(sps.pic_order_cnt_type)
                );
                assert_eq!(
                    vui_fields.aspect_ratio_info_present_flag() == 1,
                    vui.aspect_ratio_info_present_flag
                );
                assert_eq!(
                    vui_fields.timing_info_present_flag() == 1,
                    vui.timing_info_present_flag
                );
                assert_eq!(
                    vui_fields.bitstream_restriction_flag() == 1,
                    vui.bitstream_restriction_flag
                );
                assert_eq!(
                    vui_fields.motion_vectors_over_pic_boundaries_flag() == 1,
                    vui.motion_vectors_over_pic_boundaries_flag
                );
                assert_eq!(
                    vui_fields.log2_max_mv_length_horizontal(),
                    vui.log2_max_mv_length_horizontal
                );
                assert_eq!(
                    vui_fields.log2_max_mv_length_vertical(),
                    vui.log2_max_mv_length_vertical
                );
            }
        }
    }
}

#[test]
fn packed_headers_are_escaped_and_whole_bytes() {
    for profile in PROFILES {
        for geometry in &GEOMETRIES {
            let stream = stream(profile, geometry.resolution);

            for packed in [stream.sps(), stream.pps()] {
                assert_no_start_code_emulation(&packed.bytes);
                assert_eq!(packed.bits, packed.bytes.len() * 8);
                assert_ne!(*packed.bytes.last().unwrap(), 0);
            }
            for keyframe in [true, false] {
                assert_no_start_code_emulation(&stream.slice_header(keyframe).bytes);
            }
        }
    }
}

#[test]
fn horizontal_and_vertical_crop_use_two_pixel_units() {
    let stream = stream(H264Profile::High, Resolution::new(402, 226));
    let parsed = parse(&stream);

    assert_eq!(parsed.sps.width(), 416);
    assert_eq!(parsed.sps.height(), 240);
    assert_eq!(parsed.sps.frame_crop_right_offset * 2, 14);
    assert_eq!(parsed.sps.frame_crop_bottom_offset * 2, 14);
}
