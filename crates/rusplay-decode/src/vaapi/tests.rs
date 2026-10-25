#[path = "../../tests/common/mod.rs"]
mod common;

use std::path::Path;

use rusplay_config::VaapiDecoderConfig;
use rusplay_core::{Codec, EncodedFrame};

use crate::{DecodeError, DecodedBuffer, VideoDecoder, vaapi::VaapiDecoder};

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

const H264_420_8BIT_BFRAMES_SORTED: [u64; 12] = [
    0x41df_07b8_8264_5d5d,
    0x51b6_1e19_2bf6_972d,
    0x7597_2c6a_e6fa_f7d4,
    0x8048_e130_2d12_4be2,
    0x8f43_ba17_56cf_321c,
    0x9c6f_8be2_c65e_252f,
    0xb356_8186_8ece_d3ec,
    0xc269_5946_9cf0_9927,
    0xc76b_c017_a03d_eff8,
    0xdfe0_1708_7b51_c7b1,
    0xe38b_fe9f_36a0_c9cb,
    0xf1d1_cfcc_d19e_47fd,
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

const HEVC_420_8BIT_BFRAMES_SORTED: [u64; 12] = [
    0x0a7a_70c0_15b1_045e,
    0x4547_d1b2_050a_ed51,
    0x4769_b4c2_931a_41a6,
    0x6e94_576b_ea3d_60a9,
    0x7ef0_58c4_a602_f80d,
    0x92a5_ce97_619c_73b6,
    0x935f_f78a_a9d9_9e92,
    0x9a2c_9642_42db_d552,
    0x9ad3_9e71_54d9_7b6a,
    0xab1a_b8f8_a080_89c2,
    0xbd88_1a52_f212_19df,
    0xdb06_6567_6f66_b5d2,
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

const AV1_420_8BIT_ALTREF_TILES_SORTED: [u64; 12] = [
    0x0fd3_f784_725a_cde6,
    0x1976_1bf2_3eab_af27,
    0x6f55_4fc6_f8ef_9992,
    0x728a_c272_3877_3956,
    0x8dfd_2589_894c_6789,
    0xa0d1_9af8_7dca_b887,
    0xad3b_5ca8_6049_a64c,
    0xb3e0_29d3_7be5_e8c6,
    0xc64a_a9a4_1154_48d0,
    0xc8dc_e2a6_148c_a53d,
    0xfad0_c60e_d777_cbb3,
    0xff01_66ba_cf04_db1e,
];

fn fnv1a(planes: &[Vec<u8>]) -> u64 {
    planes.iter().flatten().fold(FNV_OFFSET, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    Decode,
    Any,
}

fn assert_pixels(codec: Codec, frames: &[EncodedFrame], expected: &[u64], order: Order) {
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
    for (index, frame) in frames.iter().enumerate() {
        let output = match decoder.decode(frame) {
            Ok(Some(output)) => output,
            Err(
                DecodeError::ChromaUnsupported { .. } | DecodeError::BitDepthUnsupported { .. },
            ) => {
                return;
            }
            other => panic!("frame {index}: {other:?}"),
        };
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

        hashes.push(fnv1a(&planes));
    }

    if order == Order::Any {
        hashes.sort_unstable();
    }

    assert_eq!(hashes, expected, "the decode differs from the reference");
}

#[test]
fn h264_matches_the_reference_decode() {
    assert_pixels(
        Codec::H264,
        &common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit.h264")),
        &H264_420_8BIT,
        Order::Decode,
    );
}

#[test]
fn h264_with_b_frames_slices_and_weights_matches_the_reference_decode() {
    assert_pixels(
        Codec::H264,
        &common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit-bframes.h264")),
        &H264_420_8BIT_BFRAMES_SORTED,
        Order::Any,
    );
}

#[test]
fn hevc_matches_the_reference_decode() {
    assert_pixels(
        Codec::Hevc,
        &common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-8bit.hevc")),
        &HEVC_420_8BIT,
        Order::Decode,
    );
}

#[test]
fn hevc_main10_matches_the_reference_decode() {
    assert_pixels(
        Codec::Hevc,
        &common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-10bit.hevc")),
        &HEVC_420_10BIT,
        Order::Decode,
    );
}

#[test]
fn hevc_with_b_frames_slices_and_weights_matches_the_reference_decode() {
    assert_pixels(
        Codec::Hevc,
        &common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-8bit-bframes.hevc")),
        &HEVC_420_8BIT_BFRAMES_SORTED,
        Order::Any,
    );
}

#[test]
fn av1_matches_the_reference_decode() {
    assert_pixels(
        Codec::Av1,
        &common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit.ivf")),
        &AV1_420_8BIT,
        Order::Decode,
    );
}

#[test]
fn av1_10_bit_matches_the_reference_decode() {
    assert_pixels(
        Codec::Av1,
        &common::ivf_frames(Codec::Av1, &common::fixture("av1-420-10bit.ivf")),
        &AV1_420_10BIT,
        Order::Decode,
    );
}

#[test]
fn av1_with_hidden_frames_and_tiles_matches_the_reference_decode() {
    assert_pixels(
        Codec::Av1,
        &common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit-altref.ivf")),
        &AV1_420_8BIT_ALTREF_TILES_SORTED,
        Order::Any,
    );
}
