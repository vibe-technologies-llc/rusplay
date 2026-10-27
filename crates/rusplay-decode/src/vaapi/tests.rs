mod fields;

use std::{path::Path, rc::Rc};

use cros_codecs::codec::h264::parser::{Level, Profile, Sps, SpsBuilder};
use rusplay_config::VaapiDecoderConfig;
use rusplay_core::{Codec, EncodedFrame, Timestamp};

use crate::{
    DecodeError, DecodedBuffer, DecodedFrame, VideoDecoder, fixtures as common,
    vaapi::{
        VaapiDecoder, h264,
        reorder::Depth,
        tests::fields::{Field, FieldPicture, Mmco, Modification},
    },
};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

const H264_420_8BIT: [u64; 8] = [
    0xdb00_c0c5_6736_b5cf,
    0xbf7b_b456_cebf_73d9,
    0x0b1a_2bf0_9784_9ec2,
    0x7ded_91dc_8734_d49c,
    0x305b_b263_7b71_fc99,
    0x5308_fa58_f859_7488,
    0x6a97_3522_9494_9681,
    0x073d_15da_03dd_64f1,
];

const H264_420_8BIT_BFRAMES: [u64; 12] = [
    0x9c6f_8be2_c65e_252f,
    0xdfe0_1708_7b51_c7b1,
    0x8f43_ba17_56cf_321c,
    0xb356_8186_8ece_d3ec,
    0xe38b_fe9f_36a0_c9cb,
    0x41df_07b8_8264_5d5d,
    0xf1d1_cfcc_d19e_47fd,
    0xc269_5946_9cf0_9927,
    0x8048_e130_2d12_4be2,
    0x7597_2c6a_e6fa_f7d4,
    0x51b6_1e19_2bf6_972d,
    0xc76b_c017_a03d_eff8,
];

const H264_420_8BIT_MBAFF: [u64; 12] = [
    0x813b_c8d4_18fe_df72,
    0xe7c4_d7a2_710d_a20d,
    0x5064_4368_fb72_5366,
    0x42b9_78cf_1614_cc20,
    0xbd5f_9f08_5618_8a2d,
    0x1bce_63dd_d73a_de52,
    0x38f9_2f29_168c_2a3c,
    0x2c4e_9947_3ab3_a8cd,
    0xca75_9c61_42f6_e31c,
    0xa318_ce14_2922_7e5c,
    0xe0ee_71c1_b6ed_f702,
    0xa9fd_36ad_e3e6_86d7,
];

const HEVC_420_8BIT: [u64; 8] = [
    0x6c90_154b_ff92_7d1a,
    0xe2a1_c054_8f0c_569e,
    0xc222_0e6e_ae40_3f24,
    0x2415_d4a7_37fd_bf2f,
    0x5d3d_13a7_7c76_b4df,
    0xe110_d716_ef0f_ea14,
    0xe897_e38b_517d_8a02,
    0xadce_c4a3_3a4d_bd9e,
];

const HEVC_420_10BIT: [u64; 8] = [
    0x9ec0_0710_e57e_1784,
    0x6ec8_41e3_51cf_4a9e,
    0x3263_014b_3cdb_44f9,
    0x9181_6b15_5624_8f8a,
    0x1fb0_8c6b_d0ee_5531,
    0x883b_b8a7_f1b8_4b90,
    0xe1bf_14e3_cf95_c736,
    0x63e7_075c_d70c_2bf1,
];

const HEVC_420_8BIT_BFRAMES: [u64; 12] = [
    0x9ad3_9e71_54d9_7b6a,
    0xab1a_b8f8_a080_89c2,
    0xdb06_6567_6f66_b5d2,
    0x7ef0_58c4_a602_f80d,
    0x4547_d1b2_050a_ed51,
    0x92a5_ce97_619c_73b6,
    0x0a7a_70c0_15b1_045e,
    0x9a2c_9642_42db_d552,
    0x6e94_576b_ea3d_60a9,
    0x935f_f78a_a9d9_9e92,
    0x4769_b4c2_931a_41a6,
    0xbd88_1a52_f212_19df,
];

const AV1_420_8BIT: [u64; 8] = [
    0x9b34_7b9d_32a4_f526,
    0x2741_668b_861a_557a,
    0x7a82_7dd8_bde3_3e4a,
    0xe431_86e6_ad85_6dd8,
    0x3b7b_098d_665b_9966,
    0x447a_02f7_34bc_5dbf,
    0x29ee_7664_1fd6_9214,
    0xb497_82dc_009d_1a76,
];

const AV1_420_10BIT: [u64; 8] = [
    0x9d77_07d7_bb68_d19d,
    0x1879_ce7c_790a_748b,
    0xa6a0_2f3d_014b_13d4,
    0xc080_6128_b72c_91eb,
    0xe4b9_f8cd_4d3c_b206,
    0x2f51_a193_8ff2_bc5f,
    0xa77d_4cf5_6c70_a1a2,
    0x7ed5_d341_a078_ac88,
];

const AV1_420_8BIT_ALTREF_TILES: [u64; 12] = [
    0x728a_c272_3877_3956,
    0xa0d1_9af8_7dca_b887,
    0x6f55_4fc6_f8ef_9992,
    0x1976_1bf2_3eab_af27,
    0xc8dc_e2a6_148c_a53d,
    0x0fd3_f784_725a_cde6,
    0xc64a_a9a4_1154_48d0,
    0xff01_66ba_cf04_db1e,
    0xad3b_5ca8_6049_a64c,
    0x8dfd_2589_894c_6789,
    0xb3e0_29d3_7be5_e8c6,
    0xfad0_c60e_d777_cbb3,
];

fn fnv1a(planes: &[Vec<u8>]) -> u64 {
    planes.iter().flatten().fold(FNV_OFFSET, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

fn assert_pixels(codec: Codec, frames: &[EncodedFrame], expected: &[u64], max_held: usize) {
    let config = VaapiDecoderConfig::default();
    if !Path::new(&config.device).exists() {
        return;
    }
    let mut decoder = match VaapiDecoder::new(codec, &config) {
        Ok(decoder) => decoder,
        Err(DecodeError::CodecUnsupported { .. }) => return,
        Err(error) => panic!("opening the {codec} vaapi decoder: {error}"),
    };

    let mut hashes = Vec::new();
    let mut leading_gaps = 0;
    for (index, frame) in frames.iter().enumerate() {
        let output = match decoder.decode(frame) {
            Ok(Some(output)) => output,
            Ok(None) if hashes.is_empty() => {
                leading_gaps += 1;
                continue;
            }
            Err(
                DecodeError::ChromaUnsupported { .. } | DecodeError::BitDepthUnsupported { .. },
            ) => {
                return;
            }
            other => panic!("frame {index}: {other:?}"),
        };

        hashes.push(hash_of(&decoder, &output));
    }

    assert_eq!(
        leading_gaps, max_held,
        "the decoder held a different number of frames"
    );

    let flushed = decoder.flush().unwrap();
    assert_eq!(flushed.len(), max_held);
    hashes.extend(flushed.iter().map(|output| hash_of(&decoder, output)));

    assert_eq!(hashes, expected, "the decode differs from the reference");
}

fn hash_of(decoder: &VaapiDecoder, output: &DecodedFrame) -> u64 {
    let DecodedBuffer::DmaBuf(dma_buf) = &output.buffer else {
        panic!("the frame is not a DMA-BUF");
    };

    let planes = decoder
        .display()
        .read_back(
            dma_buf.image(),
            output.resolution.width,
            output.resolution.height,
        )
        .unwrap();

    fnv1a(&planes)
}

#[test]
fn h264_matches_the_reference_decode() {
    assert_pixels(
        Codec::H264,
        &common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit.h264")),
        &H264_420_8BIT,
        0,
    );
}

#[test]
fn h264_with_b_frames_slices_and_weights_matches_the_reference_decode() {
    assert_pixels(
        Codec::H264,
        &common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit-bframes.h264")),
        &H264_420_8BIT_BFRAMES,
        2,
    );
}

#[test]
fn h264_mbaff_matches_the_reference_decode() {
    assert_pixels(
        Codec::H264,
        &common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit-mbaff.h264")),
        &H264_420_8BIT_MBAFF,
        2,
    );
}

#[test]
fn hevc_matches_the_reference_decode() {
    assert_pixels(
        Codec::Hevc,
        &common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-8bit.hevc")),
        &HEVC_420_8BIT,
        0,
    );
}

#[test]
fn hevc_main10_matches_the_reference_decode() {
    assert_pixels(
        Codec::Hevc,
        &common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-10bit.hevc")),
        &HEVC_420_10BIT,
        0,
    );
}

#[test]
fn hevc_with_b_frames_slices_and_weights_matches_the_reference_decode() {
    assert_pixels(
        Codec::Hevc,
        &common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-8bit-bframes.hevc")),
        &HEVC_420_8BIT_BFRAMES,
        2,
    );
}

#[test]
fn av1_matches_the_reference_decode() {
    assert_pixels(
        Codec::Av1,
        &common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit.ivf")),
        &AV1_420_8BIT,
        0,
    );
}

#[test]
fn av1_10_bit_matches_the_reference_decode() {
    assert_pixels(
        Codec::Av1,
        &common::ivf_frames(Codec::Av1, &common::fixture("av1-420-10bit.ivf")),
        &AV1_420_10BIT,
        0,
    );
}

#[test]
fn av1_with_hidden_frames_and_tiles_matches_the_reference_decode() {
    assert_pixels(
        Codec::Av1,
        &common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit-altref.ivf")),
        &AV1_420_8BIT_ALTREF_TILES,
        0,
    );
}

fn sps(profile: Profile, pic_order_cnt_type: u8) -> Sps {
    let built = SpsBuilder::new()
        .profile_idc(profile)
        .level_idc(Level::L4)
        .frame_mbs_only_flag(true)
        .resolution(1280, 720)
        .pic_order_cnt_type(pic_order_cnt_type)
        .build();

    Rc::into_inner(built).unwrap()
}

#[test]
fn h264_without_bitstream_restrictions_learns_the_reorder_depth() {
    let main = sps(Profile::Main, 0);

    assert_eq!(h264::reorder_depth(&main), Depth::Learned { ceiling: 9 });
}

#[test]
fn h264_without_reordering_by_construction_is_held_for_nothing() {
    assert_eq!(
        h264::reorder_depth(&sps(Profile::Baseline, 0)),
        Depth::Fixed(0)
    );
    assert_eq!(h264::reorder_depth(&sps(Profile::High, 2)), Depth::Fixed(0));
}

#[test]
fn h264_follows_the_bitstream_restrictions() {
    let mut restricted = sps(Profile::High, 0);
    restricted.vui_parameters_present_flag = true;
    restricted.vui_parameters.bitstream_restriction_flag = true;
    restricted.vui_parameters.max_num_reorder_frames = 2;
    restricted.vui_parameters.max_dec_frame_buffering = 4;

    assert_eq!(h264::reorder_depth(&restricted), Depth::Fixed(2));

    restricted.vui_parameters.max_num_reorder_frames = 0;

    assert_eq!(h264::reorder_depth(&restricted), Depth::Fixed(0));
}

fn field_packets(
    max_num_ref_frames: u32,
    reorder_frames: u32,
    pictures: &[FieldPicture],
    per_frame: bool,
) -> Vec<EncodedFrame> {
    let mut units: Vec<Vec<u8>> = pictures.iter().map(FieldPicture::encode).collect();
    units[0].splice(
        0..0,
        fields::parameter_sets(max_num_ref_frames, reorder_frames),
    );

    if per_frame {
        units = units.chunks(2).map(<[Vec<u8>]>::concat).collect();
    }

    units
        .into_iter()
        .enumerate()
        .map(|(index, data)| EncodedFrame {
            codec: Codec::H264,
            timestamp: Timestamp::from_micros(u64::try_from(index).unwrap() * 16_666),
            keyframe: index == 0,
            data,
        })
        .collect()
}

fn assert_field_stream(
    max_num_ref_frames: u32,
    pictures: &[FieldPicture],
    expected: &[(Field, Field)],
) {
    assert_reordered_field_stream(max_num_ref_frames, 0, pictures, expected);
}

fn assert_reordered_field_stream(
    max_num_ref_frames: u32,
    reorder_frames: u32,
    pictures: &[FieldPicture],
    expected: &[(Field, Field)],
) {
    let config = VaapiDecoderConfig::default();
    if !Path::new(&config.device).exists() {
        return;
    }

    for per_frame in [false, true] {
        let mut decoder = match VaapiDecoder::new(Codec::H264, &config) {
            Ok(decoder) => decoder,
            Err(DecodeError::CodecUnsupported { .. }) => return,
            Err(error) => panic!("opening the h264 vaapi decoder: {error}"),
        };

        let mut outputs = Vec::new();
        for packet in field_packets(max_num_ref_frames, reorder_frames, pictures, per_frame) {
            outputs.extend(decoder.decode(&packet).unwrap());
        }
        outputs.extend(decoder.flush().unwrap());

        assert_eq!(outputs.len(), expected.len(), "per_frame {per_frame}");
        for (index, (output, (top, bottom))) in outputs.iter().zip(expected).enumerate() {
            assert_eq!(output.resolution, fields::resolution());

            let DecodedBuffer::DmaBuf(dma_buf) = &output.buffer else {
                panic!("frame {index} is not a DMA-BUF");
            };
            let planes = decoder
                .display()
                .read_back(
                    dma_buf.image(),
                    output.resolution.width,
                    output.resolution.height,
                )
                .unwrap();

            assert!(
                planes == fields::nv12(top, bottom),
                "frame {index} differs, per_frame {per_frame}\n got {}\n want {}",
                fields::describe(&planes),
                fields::describe(&fields::nv12(top, bottom))
            );
        }
    }
}

fn intra_frame(frame: u32, frame_num: u32, top: u8, bottom: u8) -> [FieldPicture; 2] {
    [
        FieldPicture::intra(frame, frame_num, false, top),
        FieldPicture::intra(frame, frame_num, true, bottom),
    ]
}

fn probe_frame(
    frame: u32,
    frame_num: u32,
    top: (usize, Vec<Modification>),
    bottom: (usize, Vec<Modification>),
) -> [FieldPicture; 2] {
    [
        FieldPicture::probe(frame, frame_num, false, top.0).with_modifications(top.1),
        FieldPicture::probe(frame, frame_num, true, bottom.0).with_modifications(bottom.1),
    ]
}

#[test]
fn field_pairs_decode_into_one_frame_each() {
    let pictures: Vec<FieldPicture> = [intra_frame(0, 0, 1, 2), intra_frame(1, 1, 3, 4)].concat();

    assert_field_stream(
        2,
        &pictures,
        &[
            (Field::intra(1), Field::intra(2)),
            (Field::intra(3), Field::intra(4)),
        ],
    );
}

#[test]
fn field_reference_lists_alternate_parity_and_follow_the_sliding_window() {
    let pictures: Vec<FieldPicture> = [
        intra_frame(0, 0, 1, 2),
        intra_frame(1, 1, 3, 4),
        intra_frame(2, 2, 5, 6),
        probe_frame(3, 3, (6, Vec::new()), (6, Vec::new())),
        [
            FieldPicture::intra(4, 3, false, 7),
            FieldPicture::reference_probe(4, 3, true, 5),
        ],
    ]
    .concat();

    assert_field_stream(
        3,
        &pictures,
        &[
            (Field::intra(1), Field::intra(2)),
            (Field::intra(3), Field::intra(4)),
            (Field::intra(5), Field::intra(6)),
            (
                Field::probe(&[5, 6, 3, 4, 1, 2]),
                Field::probe(&[6, 5, 4, 3, 2, 1]),
            ),
            (Field::intra(7), Field::probe(&[6, 7, 4, 5, 3])),
        ],
    );
}

#[test]
fn field_reference_list_modification_counts_in_field_pic_nums() {
    let pictures: Vec<FieldPicture> = [
        intra_frame(0, 0, 1, 2),
        intra_frame(1, 1, 3, 4),
        probe_frame(
            2,
            2,
            (
                4,
                vec![
                    Modification { idc: 0, value: 4 },
                    Modification { idc: 1, value: 1 },
                ],
            ),
            (4, vec![Modification { idc: 0, value: 2 }]),
        ),
    ]
    .concat();

    assert_field_stream(
        2,
        &pictures,
        &[
            (Field::intra(1), Field::intra(2)),
            (Field::intra(3), Field::intra(4)),
            (Field::probe(&[2, 4, 3, 1]), Field::probe(&[3, 4, 2, 1])),
        ],
    );
}

fn long_term_setup(with_probe: bool) -> Vec<FieldPicture> {
    let long_term_f1 = vec![
        Mmco::MaxLongTermIndexPlus1(1),
        Mmco::ShortToLongTerm {
            difference_minus1: 1,
            index: 0,
        },
        Mmco::ShortToLongTerm {
            difference_minus1: 2,
            index: 0,
        },
    ];

    let mut pictures: Vec<FieldPicture> = [
        intra_frame(0, 0, 1, 2),
        intra_frame(1, 1, 3, 4),
        [
            FieldPicture::intra(2, 2, false, 5).with_marking(long_term_f1),
            FieldPicture::intra(2, 2, true, 6),
        ],
    ]
    .concat();
    if with_probe {
        pictures.extend(probe_frame(3, 3, (6, Vec::new()), (6, Vec::new())));
    }
    pictures
}

fn frame_after_setup(with_probe: bool, operations: Vec<Mmco>) -> Vec<FieldPicture> {
    let mut pictures = long_term_setup(with_probe);
    pictures.extend([
        FieldPicture::intra(4, 3, false, 7).with_marking(operations),
        FieldPicture::intra(4, 3, true, 8),
    ]);
    pictures.extend(probe_frame(5, 4, (6, Vec::new()), (6, Vec::new())));
    pictures
}

fn setup_frames(with_probe: bool) -> Vec<(Field, Field)> {
    let mut frames = vec![
        (Field::intra(1), Field::intra(2)),
        (Field::intra(3), Field::intra(4)),
        (Field::intra(5), Field::intra(6)),
    ];
    if with_probe {
        frames.push((
            Field::probe(&[5, 6, 1, 2, 3, 4]),
            Field::probe(&[6, 5, 2, 1, 4, 3]),
        ));
    }
    frames.push((Field::intra(7), Field::intra(8)));
    frames
}

#[test]
fn field_marking_orders_long_term_pairs_after_short_term_ones() {
    let mut expected = setup_frames(true);
    expected.push((
        Field::probe(&[7, 8, 1, 6, 2, 3]),
        Field::probe(&[8, 7, 6, 1, 2, 4]),
    ));
    let unmark_short_term_field = vec![Mmco::UnmarkShortTerm {
        difference_minus1: 1,
    }];

    assert_field_stream(
        4,
        &frame_after_setup(true, unmark_short_term_field),
        &expected,
    );
}

#[test]
fn field_marking_unmarks_a_single_long_term_field() {
    let mut expected = setup_frames(false);
    expected.push((
        Field::probe(&[7, 8, 5, 6, 1, 2]),
        Field::probe(&[8, 7, 6, 5, 2, 1]),
    ));
    let unmark_long_term_field = vec![Mmco::UnmarkLongTerm {
        long_term_pic_num: 1,
    }];

    assert_field_stream(
        4,
        &frame_after_setup(false, unmark_long_term_field),
        &expected,
    );
}

#[test]
#[ignore = "decode-vaapi-driver-field-long-term: the vcn firmware mis-marks the long-term field"]
fn field_marking_unmarks_a_long_term_field_after_a_non_reference_frame() {
    let mut expected = setup_frames(true);
    expected.push((
        Field::probe(&[7, 8, 5, 6, 1, 2, 4]),
        Field::probe(&[8, 7, 6, 5, 2, 1, 4]),
    ));
    let unmark_long_term_field = vec![Mmco::UnmarkLongTerm {
        long_term_pic_num: 1,
    }];

    let mut pictures = long_term_setup(true);
    pictures.extend([
        FieldPicture::intra(4, 3, false, 7).with_marking(unmark_long_term_field),
        FieldPicture::intra(4, 3, true, 8),
    ]);
    pictures.extend(probe_frame(5, 4, (7, Vec::new()), (7, Vec::new())));

    assert_field_stream(4, &pictures, &expected);
}

#[test]
fn an_unpaired_field_is_output_as_its_own_frame() {
    let config = VaapiDecoderConfig::default();
    if !Path::new(&config.device).exists() {
        return;
    }
    let Ok(mut decoder) = VaapiDecoder::new(Codec::H264, &config) else {
        return;
    };

    let pictures = [
        FieldPicture::intra(0, 0, false, 1),
        FieldPicture::intra(0, 0, true, 2),
        FieldPicture::intra(1, 1, false, 3),
        FieldPicture::intra(2, 2, false, 5),
        FieldPicture::intra(2, 2, true, 6),
        FieldPicture::intra(3, 3, false, 7),
    ];

    let mut outputs = Vec::new();
    for packet in field_packets(2, 0, &pictures, false) {
        outputs.extend(decoder.decode(&packet).unwrap());
    }
    outputs.extend(decoder.flush().unwrap());

    assert_eq!(outputs.len(), 4);

    let top_rows = |output: &DecodedFrame| -> Vec<u8> {
        let DecodedBuffer::DmaBuf(dma_buf) = &output.buffer else {
            panic!("the frame is not a DMA-BUF");
        };
        let planes = decoder
            .display()
            .read_back(dma_buf.image(), 128, 64)
            .unwrap();
        planes[0]
            .chunks(128)
            .step_by(2)
            .flatten()
            .copied()
            .collect()
    };
    let expected = |seed: u8| -> Vec<u8> {
        fields::nv12(&Field::intra(seed), &Field::intra(seed))[0]
            .chunks(128)
            .step_by(2)
            .flatten()
            .copied()
            .collect()
    };

    assert_eq!(top_rows(&outputs[0]), expected(1));
    assert_eq!(top_rows(&outputs[1]), expected(3));
    assert_eq!(top_rows(&outputs[2]), expected(5));
    assert_eq!(top_rows(&outputs[3]), expected(7));
}

#[test]
fn field_b_pictures_build_both_lists_around_the_current_order_count() {
    let pictures = [
        intra_frame(0, 0, 1, 2),
        intra_frame(4, 1, 3, 4),
        [
            FieldPicture::bidirectional_probe(2, 2, false, 4, 4),
            FieldPicture::bidirectional_probe(2, 2, true, 4, 4),
        ],
    ]
    .concat();

    assert_reordered_field_stream(
        2,
        1,
        &pictures,
        &[
            (Field::intra(1), Field::intra(2)),
            (
                Field::bidirectional_probe(&[1, 2, 3, 4], &[3, 4, 1, 2]),
                Field::bidirectional_probe(&[2, 1, 4, 3], &[4, 3, 2, 1]),
            ),
            (Field::intra(3), Field::intra(4)),
        ],
    );
}
