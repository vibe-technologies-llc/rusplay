use std::rc::Rc;

use cros_codecs::codec::av1::parser::{
    BitDepth as ParsedDepth, ChromaSamplePosition, FrameHeaderObu, FrameType, InterpolationFilter,
    ObuAction, ObuType, ParsedObu, Parser, Profile, SequenceHeaderObu, TxMode,
};
use rusplay_core::{BitDepth, ChromaSubsampling, FrameRate, PixelFormat, Resolution};

use super::{bits_at, color_cases, every_format, frame_rates, picture, pixel, stream_info};
use crate::{
    obu::temporal_delimiter,
    vaapi::{
        Picture,
        av1::{Av1Profile, Av1Stream, TileLayout},
        va::INVALID_SURFACE,
    },
};

const FRAME_HEADER_OBU_BYTE: u8 = 0x1a;
const SEQUENCE_HEADER_OBU_BYTE: u8 = 0x0a;
const RESOLUTIONS: [Resolution; 6] = [
    Resolution::new(402, 226),
    Resolution::new(1920, 1080),
    Resolution::new(1280, 720),
    Resolution::new(400, 216),
    Resolution::new(64, 64),
    Resolution::new(3840, 2160),
];

struct Supported {
    format: PixelFormat,
    profile: Av1Profile,
    parsed_profile: Profile,
    depth: ParsedDepth,
    subsampled: bool,
}

fn supported() -> Vec<Supported> {
    let case = |chroma, depth, profile, parsed_profile, parsed_depth, subsampled| Supported {
        format: pixel(chroma, depth),
        profile,
        parsed_profile,
        depth: parsed_depth,
        subsampled,
    };
    let (c420, c444) = (ChromaSubsampling::Yuv420, ChromaSubsampling::Yuv444);

    vec![
        case(
            c420,
            BitDepth::Eight,
            Av1Profile::Main,
            Profile::Profile0,
            ParsedDepth::Depth8,
            true,
        ),
        case(
            c420,
            BitDepth::Ten,
            Av1Profile::Main,
            Profile::Profile0,
            ParsedDepth::Depth10,
            true,
        ),
        case(
            c444,
            BitDepth::Eight,
            Av1Profile::High,
            Profile::Profile1,
            ParsedDepth::Depth8,
            false,
        ),
        case(
            c444,
            BitDepth::Ten,
            Av1Profile::High,
            Profile::Profile1,
            ParsedDepth::Depth10,
            false,
        ),
    ]
}

struct Parsed {
    sequence: Rc<SequenceHeaderObu>,
    frames: Vec<FrameHeaderObu>,
}

fn feed(
    parser: &mut Parser,
    data: &[u8],
) -> (Option<Rc<SequenceHeaderObu>>, Option<FrameHeaderObu>) {
    let mut sequence = None;
    let mut frame = None;
    let mut rest = data;

    while !rest.is_empty() {
        let ObuAction::Process(obu) = parser.read_obu(rest).unwrap() else {
            panic!("dropped an obu");
        };
        rest = &rest[obu.bytes_used..];
        match parser.parse_obu(obu).unwrap() {
            ParsedObu::SequenceHeader(header) => sequence = Some(header),
            ParsedObu::FrameHeader(header) => {
                parser.ref_frame_update(&header).unwrap();
                frame = Some(header);
            }
            _ => {}
        }
    }

    (sequence, frame)
}

fn parse(stream: &Av1Stream, frames: &[(bool, u8)]) -> Parsed {
    let mut parser = Parser::default();
    let mut sequence = None;
    let mut headers = Vec::new();

    for &(keyframe, order_hint) in frames {
        let mut data = if keyframe {
            stream.sequence_header()
        } else {
            temporal_delimiter()
        };
        data.extend(stream.frame_header(keyframe, order_hint).0);
        let (parsed_sequence, frame) = feed(&mut parser, &data);
        sequence = sequence.or(parsed_sequence);
        headers.push(frame.unwrap());
    }

    Parsed {
        sequence: sequence.unwrap(),
        frames: headers,
    }
}

fn stream(case: &Supported, resolution: Resolution) -> Av1Stream {
    Av1Stream::new(case.profile, stream_info(case.format, resolution))
}

fn expected_qindex(qp: Option<u8>) -> u32 {
    match qp {
        None => 128,
        Some(0) => 1,
        Some(qp) => (u32::from(qp) * 5).min(255),
    }
}

#[test]
fn only_the_documented_formats_have_a_profile() {
    for format in every_format() {
        let expected = supported()
            .into_iter()
            .find(|case| case.format == format)
            .map(|case| case.profile);

        assert_eq!(Av1Profile::for_format(format), expected, "{format:?}");
    }
}

#[test]
fn sequence_header_carries_every_field_the_writer_sets() {
    for case in supported() {
        for resolution in RESOLUTIONS {
            let stream = stream(&case, resolution);
            let bytes = stream.sequence_header();
            let parsed = parse(&stream, &[(true, 0)]);
            let sequence = &parsed.sequence;
            let width_bits = 32 - (resolution.width - 1).leading_zeros();
            let height_bits = 32 - (resolution.height - 1).leading_zeros();

            assert_eq!(bytes[0], SEQUENCE_HEADER_OBU_BYTE);
            assert_eq!(usize::from(bytes[1]), bytes.len() - 2);
            assert_ne!(*bytes.last().unwrap(), 0);
            assert_eq!(sequence.seq_profile, case.parsed_profile);
            assert!(!sequence.still_picture);
            assert!(!sequence.reduced_still_picture_header);
            assert!(!sequence.timing_info_present_flag);
            assert!(!sequence.decoder_model_info_present_flag);
            assert!(!sequence.initial_display_delay_present_flag);
            assert_eq!(sequence.operating_points_cnt_minus_1, 0);
            assert_eq!(sequence.operating_points[0].idc, 0);
            assert_eq!(u32::from(sequence.frame_width_bits_minus_1) + 1, width_bits);
            assert_eq!(
                u32::from(sequence.frame_height_bits_minus_1) + 1,
                height_bits
            );
            assert_eq!(
                u32::from(sequence.max_frame_width_minus_1) + 1,
                resolution.width
            );
            assert_eq!(
                u32::from(sequence.max_frame_height_minus_1) + 1,
                resolution.height
            );
            assert!(!sequence.frame_id_numbers_present_flag);
            assert!(!sequence.use_128x128_superblock);
            assert!(!sequence.enable_filter_intra);
            assert!(!sequence.enable_intra_edge_filter);
            assert!(!sequence.enable_interintra_compound);
            assert!(!sequence.enable_masked_compound);
            assert!(!sequence.enable_warped_motion);
            assert!(!sequence.enable_dual_filter);
            assert!(sequence.enable_order_hint);
            assert!(!sequence.enable_jnt_comp);
            assert!(!sequence.enable_ref_frame_mvs);
            assert!(!sequence.seq_choose_screen_content_tools);
            assert_eq!(sequence.seq_force_screen_content_tools, 0);
            assert_eq!(sequence.order_hint_bits_minus_1, 7);
            assert_eq!(sequence.order_hint_bits, 8);
            assert!(!sequence.enable_superres);
            assert!(sequence.enable_cdef);
            assert!(!sequence.enable_restoration);
            assert!(!sequence.film_grain_params_present);
            assert_eq!(sequence.bit_depth, case.depth);
            assert_eq!(sequence.num_planes, 3);
        }
    }
}

#[test]
fn colour_config_signals_the_colour_description() {
    for case in supported() {
        for colour in color_cases() {
            let mut info = stream_info(case.format, Resolution::new(1920, 1080));
            info.color = colour.color;
            let stream = Av1Stream::new(case.profile, info);
            let config = parse(&stream, &[(true, 0)]).sequence.color_config.clone();

            assert_eq!(config.high_bitdepth, case.format.depth != BitDepth::Eight);
            assert!(!config.twelve_bit);
            assert!(!config.mono_chrome);
            assert!(config.color_description_present_flag);
            assert_eq!(config.color_primaries as u8, colour.primaries);
            assert_eq!(config.transfer_characteristics as u8, colour.transfer);
            assert_eq!(config.matrix_coefficients as u8, colour.matrix);
            assert_eq!(config.color_range, colour.full_range);
            assert_eq!(config.subsampling_x, case.subsampled);
            assert_eq!(config.subsampling_y, case.subsampled);
            let position = if case.subsampled {
                ChromaSamplePosition::Vertical
            } else {
                ChromaSamplePosition::Unknown
            };
            assert_eq!(config.chroma_sample_position, position);
            assert!(!config.separate_uv_delta_q);
        }
    }
}

#[test]
fn level_and_tier_follow_resolution_rate_bitrate_and_profile() {
    let cases = [
        (Av1Profile::Main, 1920, 1080, 60, 20_000, 9, false),
        (Av1Profile::Main, 1920, 1080, 60, 25_000, 9, true),
        (Av1Profile::Main, 400, 216, 30, 5_000, 4, false),
        (Av1Profile::High, 400, 216, 30, 5_000, 1, false),
        (Av1Profile::Main, 400, 216, 30, 1_000, 0, false),
        (Av1Profile::Main, 3840, 2160, 60, 40_000, 13, false),
        (Av1Profile::Main, 3840, 2160, 60, 45_000, 13, true),
    ];

    for (profile, width, height, fps, kbps, level, high_tier) in cases {
        let format = match profile {
            Av1Profile::Main => pixel(ChromaSubsampling::Yuv420, BitDepth::Eight),
            Av1Profile::High => pixel(ChromaSubsampling::Yuv444, BitDepth::Eight),
        };
        let mut info = stream_info(format, Resolution::new(width, height));
        info.frame_rate = FrameRate::integer(fps);
        info.peak_kbps = kbps;
        let stream = Av1Stream::new(profile, info);

        let sequence = parse(&stream, &[(true, 0)]).sequence;
        let parameters = stream.sequence_parameters(1_000);
        let label = format!("{profile:?} {width}x{height}@{fps} {kbps}");

        assert_eq!(sequence.operating_points[0].seq_level_idx, level, "{label}");
        assert_eq!(
            sequence.operating_points[0].seq_tier == 1,
            high_tier,
            "{label}"
        );
        assert_eq!(parameters.seq_level_idx, level, "{label}");
        assert_eq!(parameters.seq_tier == 1, high_tier, "{label}");
    }
}

#[test]
fn key_frame_header_carries_every_field_the_writer_sets() {
    for case in supported() {
        for resolution in RESOLUTIONS {
            for qp in [None, Some(0), Some(10), Some(30), Some(51), Some(80)] {
                let mut info = stream_info(case.format, resolution);
                info.qp = qp;
                let stream = Av1Stream::new(case.profile, info);
                let parsed = parse(&stream, &[(true, 0)]);
                let header = &parsed.frames[0];

                assert_eq!(header.obu_header.obu_type, ObuType::FrameHeader);
                assert!(!header.show_existing_frame);
                assert_eq!(header.frame_type, FrameType::KeyFrame);
                assert!(header.show_frame);
                assert!(header.error_resilient_mode);
                assert!(!header.disable_cdf_update);
                assert_eq!(header.allow_screen_content_tools, 0);
                assert!(!header.frame_size_override_flag);
                assert_eq!(header.order_hint, 0);
                assert_eq!(header.primary_ref_frame, 7);
                assert_eq!(header.refresh_frame_flags, 0xff);
                assert!(!header.allow_intrabc);
                assert!(header.render_and_frame_size_different);
                assert!(!header.use_superres);
                assert_eq!(header.frame_width, resolution.width);
                assert_eq!(header.frame_height, resolution.height);
                assert_eq!(header.upscaled_width, resolution.width);
                assert_eq!(header.render_width, resolution.width);
                assert_eq!(header.render_height, resolution.height);
                assert!(!header.disable_frame_end_update_cdf);
                assert_eq!(header.quantization_params.base_q_idx, expected_qindex(qp));
                assert!(!header.quantization_params.diff_uv_delta);
                assert!(!header.quantization_params.using_qmatrix);
                assert!(!header.quantization_params.delta_q_present);
                assert_eq!(header.quantization_params.delta_q_y_dc, 0);
                assert_eq!(header.quantization_params.delta_q_u_dc, 0);
                assert_eq!(header.quantization_params.delta_q_u_ac, 0);
                assert!(!header.segmentation_params.segmentation_enabled);
                assert_eq!(header.loop_filter_params.loop_filter_level, [0; 4]);
                assert_eq!(header.loop_filter_params.loop_filter_sharpness, 0);
                assert!(!header.loop_filter_params.loop_filter_delta_enabled);
                assert_eq!(header.cdef_params.cdef_bits, 0);
                assert_eq!(header.cdef_params.cdef_y_pri_strength[0], 0);
                assert_eq!(header.cdef_params.cdef_y_sec_strength[0], 0);
                assert_eq!(header.cdef_params.cdef_uv_pri_strength[0], 0);
                assert_eq!(header.cdef_params.cdef_uv_sec_strength[0], 0);
                assert_eq!(header.tx_mode, TxMode::Select);
                assert!(!header.reduced_tx_set);
                assert!(header.tile_info.uniform_tile_spacing_flag);
            }
        }
    }
}

#[test]
fn inter_frame_header_carries_every_field_the_writer_sets() {
    for case in supported() {
        for resolution in RESOLUTIONS {
            let stream = stream(&case, resolution);
            let parsed = parse(&stream, &[(true, 0), (false, 1), (false, 2), (false, 255)]);

            for (header, order_hint) in parsed.frames[1..].iter().zip([1, 2, 255]) {
                assert_eq!(header.obu_header.obu_type, ObuType::FrameHeader);
                assert!(!header.show_existing_frame);
                assert_eq!(header.frame_type, FrameType::InterFrame);
                assert!(header.show_frame);
                assert!(!header.error_resilient_mode);
                assert!(!header.disable_cdf_update);
                assert_eq!(header.allow_screen_content_tools, 0);
                assert!(!header.frame_size_override_flag);
                assert_eq!(header.order_hint, order_hint);
                assert_eq!(header.primary_ref_frame, 0);
                assert_eq!(header.refresh_frame_flags, 1);
                assert!(!header.frame_refs_short_signaling);
                assert_eq!(header.ref_frame_idx, [0; 7]);
                assert!(!header.allow_high_precision_mv);
                assert!(!header.is_filter_switchable);
                assert_eq!(header.interpolation_filter, InterpolationFilter::EightTap);
                assert!(!header.is_motion_mode_switchable);
                assert!(!header.use_ref_frame_mvs);
                assert!(!header.disable_frame_end_update_cdf);
                assert!(!header.reference_select);
                assert!(!header.skip_mode_present);
                assert!(!header.allow_warped_motion);
                assert!(!header.reduced_tx_set);
                assert!(header.render_and_frame_size_different);
                assert_eq!(header.frame_width, resolution.width);
                assert_eq!(header.frame_height, resolution.height);
                assert_eq!(header.render_width, resolution.width);
                assert_eq!(header.render_height, resolution.height);
                assert_eq!(header.quantization_params.base_q_idx, 128);
                assert_eq!(header.tx_mode, TxMode::Select);
            }
        }
    }
}

#[test]
fn order_hint_wraps_at_eight_bits() {
    let case = &supported()[0];
    let stream = stream(case, Resolution::new(402, 226));

    for (index, hint) in [(1, 1), (255, 255), (256, 0), (257, 1), (1000, 232)] {
        let expected = Av1Stream::order_hint(&picture(index, false));
        let parsed = parse(&stream, &[(true, 0), (false, expected)]);

        assert_eq!(expected, hint);
        assert_eq!(parsed.frames[1].order_hint, u32::from(hint));
    }
}

#[test]
fn frame_header_obu_framing_and_bit_offsets_point_at_the_fields() {
    for case in supported() {
        for resolution in RESOLUTIONS {
            for qp in [None, Some(7), Some(30), Some(51)] {
                let mut info = stream_info(case.format, resolution);
                info.qp = qp;
                let stream = Av1Stream::new(case.profile, info);

                for keyframe in [true, false] {
                    let (bytes, offsets) = stream.frame_header(keyframe, 5);
                    let payload = bytes.len() - 3;

                    assert_eq!(bytes[0], FRAME_HEADER_OBU_BYTE);
                    assert_eq!(
                        usize::from(bytes[1] & 0x7f) | (usize::from(bytes[2]) << 7),
                        payload
                    );
                    assert_ne!(bytes[1] & 0x80, 0);
                    assert_eq!(bytes[2] & 0x80, 0);
                    assert_ne!(*bytes.last().unwrap(), 0);
                    assert_eq!(offsets.total_bits as usize, bytes.len() * 8);
                    assert_eq!(bits_at(&bytes, offsets.qindex, 8), expected_qindex(qp));
                    assert_eq!(offsets.segmentation, offsets.qindex + 8 + 4);
                    assert_eq!(bits_at(&bytes, offsets.segmentation, 2), 0);
                    assert_eq!(offsets.loop_filter, offsets.segmentation + 2);
                    assert_eq!(bits_at(&bytes, offsets.loop_filter, 16), 0);
                    assert_eq!(offsets.cdef, offsets.loop_filter + 16);
                    assert_eq!(offsets.cdef_bits, 16);
                    assert_eq!(bits_at(&bytes, offsets.cdef, 16), 0);
                    assert_eq!(bits_at(&bytes, offsets.cdef + offsets.cdef_bits, 1), 1);
                    assert!(offsets.qindex > 3 * 8);
                }
            }
        }
    }
}

#[test]
fn tiles_split_wide_frames_consistently_with_the_header() {
    let resolutions = [
        Resolution::new(402, 226),
        Resolution::new(1920, 1080),
        Resolution::new(4200, 1000),
        Resolution::new(5120, 2880),
        Resolution::new(7680, 4320),
    ];
    let case = &supported()[0];

    for resolution in resolutions {
        let mut stream = stream(case, resolution);
        let parsed = parse(&stream, &[(true, 0)]);
        let tiles = &parsed.frames[0].tile_info;
        let key = picture(0, true);
        stream.begin(&key);
        let (_, offsets) = stream.frame_header(true, 0);
        let parameters = stream.picture_parameters(&key, 0, offsets);
        let mi_cols = 2 * resolution.width.div_ceil(8);
        let mi_rows = 2 * resolution.height.div_ceil(8);

        assert_eq!(
            TileLayout::new(resolution).tile_count(),
            (tiles.tile_cols * tiles.tile_rows) as usize
        );
        assert_eq!(u32::from(parameters.tile_cols), tiles.tile_cols);
        assert_eq!(u32::from(parameters.tile_rows), tiles.tile_rows);
        assert_eq!(tiles.mi_col_starts[tiles.tile_cols as usize], mi_cols);
        assert_eq!(tiles.mi_row_starts[tiles.tile_rows as usize], mi_rows);
        for column in 0..tiles.tile_cols as usize {
            let sbs = (tiles.mi_col_starts[column + 1] - tiles.mi_col_starts[column]).div_ceil(16);
            assert_eq!(u32::from(parameters.width_in_sbs_minus_1[column]) + 1, sbs);
        }
        for row in 0..tiles.tile_rows as usize {
            let sbs = (tiles.mi_row_starts[row + 1] - tiles.mi_row_starts[row]).div_ceil(16);
            assert_eq!(u32::from(parameters.height_in_sbs_minus_1[row]) + 1, sbs);
        }
        if tiles.tile_cols * tiles.tile_rows > 1 {
            assert_eq!(tiles.tile_size_bytes, 4);
        }
    }

    assert_eq!(TileLayout::new(Resolution::new(1920, 1080)).tile_count(), 1);
    assert_eq!(TileLayout::new(Resolution::new(7680, 4320)).tile_count(), 4);
}

#[test]
fn sequence_parameters_agree_with_the_sequence_header() {
    for case in supported() {
        for resolution in RESOLUTIONS {
            for frame_rate in frame_rates() {
                let mut info = stream_info(case.format, resolution);
                info.frame_rate = frame_rate;
                let stream = Av1Stream::new(case.profile, info);
                let sequence = parse(&stream, &[(true, 0)]).sequence;

                let parameters = stream.sequence_parameters(7_654_321);
                let fields = unsafe { parameters.seq_fields.bits };

                assert_eq!(parameters.seq_profile, sequence.seq_profile as u8);
                assert_eq!(
                    parameters.seq_level_idx,
                    sequence.operating_points[0].seq_level_idx
                );
                assert_eq!(parameters.seq_tier, sequence.operating_points[0].seq_tier);
                assert_eq!(parameters.intra_period, 0xffff);
                assert_eq!(parameters.ip_period, 1);
                assert_eq!(parameters.bits_per_second, 7_654_321);
                assert_eq!(
                    i32::from(parameters.order_hint_bits_minus_1),
                    sequence.order_hint_bits_minus_1
                );
                assert_eq!(fields.enable_order_hint() == 1, sequence.enable_order_hint);
                assert_eq!(fields.enable_cdef() == 1, sequence.enable_cdef);
                assert_eq!(
                    fields.enable_restoration() == 1,
                    sequence.enable_restoration
                );
                assert_eq!(fields.enable_superres() == 1, sequence.enable_superres);
                assert_eq!(
                    fields.use_128x128_superblock() == 1,
                    sequence.use_128x128_superblock
                );
                assert_eq!(
                    fields.bit_depth_minus8(),
                    match sequence.bit_depth {
                        ParsedDepth::Depth8 => 0,
                        ParsedDepth::Depth10 => 2,
                        ParsedDepth::Depth12 => 4,
                    }
                );
                assert_eq!(
                    fields.subsampling_x() == 1,
                    sequence.color_config.subsampling_x
                );
                assert_eq!(
                    fields.subsampling_y() == 1,
                    sequence.color_config.subsampling_y
                );
                assert_eq!(fields.mono_chrome() == 1, sequence.color_config.mono_chrome);
            }
        }
    }
}

#[test]
fn picture_parameters_follow_the_stream_state() {
    let gop = [(0, true), (1, false), (2, false), (0, true), (1, false)];

    for case in supported() {
        for resolution in RESOLUTIONS {
            for qp in [None, Some(30)] {
                let mut info = stream_info(case.format, resolution);
                info.qp = qp;
                let mut stream = Av1Stream::new(case.profile, info);
                let mut previous: Option<Picture> = None;

                for (position, (index, keyframe)) in gop.into_iter().enumerate() {
                    let picture = Picture {
                        recon: 200 + u32::try_from(position).unwrap(),
                        ..picture(index, keyframe)
                    };
                    stream.begin(&picture);
                    let order_hint = Av1Stream::order_hint(&picture);
                    let (bytes, offsets) = stream.frame_header(keyframe, order_hint);
                    let parameters = stream.picture_parameters(&picture, order_hint, offsets);
                    let flags = unsafe { parameters.picture_flags.bits };
                    let modes = unsafe { parameters.mode_control_flags.bits };
                    let tile_group = unsafe { parameters.tile_group_obu_hdr_info.bits };
                    let header = parse(&stream, &[(true, 0), (keyframe, order_hint)])
                        .frames
                        .pop()
                        .unwrap();

                    assert_eq!(
                        u32::from(parameters.frame_width_minus_1) + 1,
                        resolution.width
                    );
                    assert_eq!(
                        u32::from(parameters.frame_height_minus_1) + 1,
                        resolution.height
                    );
                    assert_eq!(parameters.reconstructed_frame, picture.recon);
                    assert_eq!(parameters.coded_buf, picture.coded);
                    assert_eq!(u32::from(parameters.order_hint), header.order_hint);
                    assert_eq!(
                        u32::from(parameters.primary_ref_frame),
                        header.primary_ref_frame
                    );
                    assert_eq!(
                        u32::from(parameters.refresh_frame_flags),
                        header.refresh_frame_flags
                    );
                    assert_eq!(
                        u32::from(parameters.base_qindex),
                        header.quantization_params.base_q_idx
                    );
                    assert_eq!(parameters.min_base_qindex, 1);
                    assert_eq!(parameters.max_base_qindex, 255);
                    assert_eq!(parameters.bit_offset_qindex, offsets.qindex);
                    assert_eq!(parameters.bit_offset_segmentation, offsets.segmentation);
                    assert_eq!(parameters.bit_offset_loopfilter_params, offsets.loop_filter);
                    assert_eq!(parameters.bit_offset_cdef_params, offsets.cdef);
                    assert_eq!(parameters.size_in_bits_cdef_params, offsets.cdef_bits);
                    assert_eq!(parameters.byte_offset_frame_hdr_obu_size, 1);
                    assert_eq!(parameters.size_in_bits_frame_hdr_obu, offsets.total_bits);
                    assert_eq!(offsets.total_bits as usize, bytes.len() * 8);
                    assert_eq!(flags.frame_type(), header.frame_type as u32);
                    assert_eq!(
                        flags.error_resilient_mode() == 1,
                        header.error_resilient_mode
                    );
                    assert_eq!(modes.tx_mode(), header.tx_mode as u32);
                    assert_eq!(tile_group.obu_has_size_field(), 1);

                    if keyframe {
                        assert_eq!(parameters.reference_frames, [INVALID_SURFACE; 8]);
                        assert_eq!(unsafe { parameters.ref_frame_ctrl_l0.value }, 0);
                    } else {
                        let previous = previous.unwrap();

                        assert_eq!(parameters.reference_frames[0], previous.recon);
                        assert_eq!(parameters.reference_frames[1..], [INVALID_SURFACE; 7]);
                        assert_eq!(unsafe { parameters.ref_frame_ctrl_l0.value }, 1);
                    }

                    stream.commit(&picture);
                    previous = Some(picture);
                }
            }
        }
    }
}
