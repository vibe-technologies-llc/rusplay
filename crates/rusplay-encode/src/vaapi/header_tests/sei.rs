use rusplay_core::{ContentLightLevel, HdrLevels};

use super::assert_no_start_code_emulation;
use crate::{
    hdr::fixtures::hdr10_color,
    vaapi::hdr::{SeiNal, metadata_obus, sei},
};

const MASTERING_SEI_PAYLOAD: [u8; 24] = [
    0x33, 0xc2, 0x86, 0xc4, 0x1d, 0x4c, 0x0b, 0xb8, 0x84, 0xd0, 0x3e, 0x80, 0x3d, 0x13, 0x40, 0x42,
    0x00, 0x98, 0x96, 0x80, 0x00, 0x00, 0x00, 0x32,
];
const MASTERING_SEI_ESCAPED: [u8; 25] = [
    0x33, 0xc2, 0x86, 0xc4, 0x1d, 0x4c, 0x0b, 0xb8, 0x84, 0xd0, 0x3e, 0x80, 0x3d, 0x13, 0x40, 0x42,
    0x00, 0x98, 0x96, 0x80, 0x00, 0x00, 0x03, 0x00, 0x32,
];
const MASTERING_AV1_PAYLOAD: [u8; 24] = [
    0xae, 0x14, 0x51, 0xec, 0x43, 0xd7, 0xb0, 0xa4, 0x26, 0x66, 0x0f, 0x5c, 0x50, 0x0d, 0x54, 0x39,
    0x00, 0x03, 0xe8, 0x00, 0x00, 0x00, 0x00, 0x52,
];
const CONTENT_LIGHT_PAYLOAD: [u8; 4] = [0x03, 0xe8, 0x01, 0x90];
const SEI_MASTERING_DISPLAY: u8 = 137;
const SEI_CONTENT_LIGHT: u8 = 144;
const RBSP_STOP_BYTE: u8 = 0x80;
const AV1_METADATA_HEADER: u8 = 0x2a;
const AV1_METADATA_CONTENT_LIGHT: u8 = 1;
const AV1_METADATA_MASTERING_DISPLAY: u8 = 2;

fn hdr(mastering: bool, content_light: bool) -> HdrLevels {
    let full = hdr10_color().hdr;

    HdrLevels {
        mastering: full.mastering.filter(|_| mastering),
        content_light: full.content_light.filter(|_| content_light),
        ..full
    }
}

fn expected_sei_rbsp(mastering: bool, content_light: bool) -> Vec<u8> {
    let mut rbsp = Vec::new();
    if mastering {
        rbsp.extend([SEI_MASTERING_DISPLAY, 24]);
        rbsp.extend(MASTERING_SEI_ESCAPED);
    }
    if content_light {
        rbsp.extend([SEI_CONTENT_LIGHT, 4]);
        rbsp.extend(CONTENT_LIGHT_PAYLOAD);
    }
    rbsp.push(RBSP_STOP_BYTE);
    rbsp
}

fn messages(rbsp_bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut unescaped = Vec::new();
    let mut zeros = 0;
    for &byte in rbsp_bytes {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        unescaped.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }

    let mut messages = Vec::new();
    let mut rest = unescaped.as_slice();
    while rest.len() > 1 {
        let size = usize::from(rest[1]);
        messages.push((rest[0], rest[2..2 + size].to_vec()));
        rest = &rest[2 + size..];
    }
    assert_eq!(rest, [RBSP_STOP_BYTE]);

    messages
}

#[test]
fn streams_without_hdr_levels_carry_no_metadata() {
    let none = HdrLevels::default();

    assert!(sei(SeiNal::H264, &none).is_none());
    assert!(sei(SeiNal::Hevc, &none).is_none());
    assert!(metadata_obus(&none).is_none());
}

#[test]
fn h264_sei_is_one_nal_with_the_messages_in_order() {
    for (mastering, content_light) in [(true, true), (true, false), (false, true)] {
        let packed = sei(SeiNal::H264, &hdr(mastering, content_light)).unwrap();
        let expected = [
            vec![0, 0, 0, 1, 0x06],
            expected_sei_rbsp(mastering, content_light),
        ]
        .concat();

        assert_eq!(
            packed.bytes, expected,
            "mastering {mastering} light {content_light}"
        );
        assert_eq!(packed.bits, packed.bytes.len() * 8);
        assert_no_start_code_emulation(&packed.bytes);
    }
}

#[test]
fn hevc_sei_is_one_prefix_nal_with_the_messages_in_order() {
    for (mastering, content_light) in [(true, true), (true, false), (false, true)] {
        let packed = sei(SeiNal::Hevc, &hdr(mastering, content_light)).unwrap();
        let expected = [
            vec![0, 0, 0, 1, 0x4e, 0x01],
            expected_sei_rbsp(mastering, content_light),
        ]
        .concat();

        assert_eq!(
            packed.bytes, expected,
            "mastering {mastering} light {content_light}"
        );
        assert_eq!(packed.bits, packed.bytes.len() * 8);
        assert_no_start_code_emulation(&packed.bytes);
    }
}

#[test]
fn sei_messages_carry_the_st2086_and_cta861_payloads() {
    for (nal, header_len) in [(SeiNal::H264, 5), (SeiNal::Hevc, 6)] {
        let packed = sei(nal, &hdr(true, true)).unwrap();
        let parsed = messages(&packed.bytes[header_len..]);

        assert_eq!(
            parsed,
            vec![
                (SEI_MASTERING_DISPLAY, MASTERING_SEI_PAYLOAD.to_vec()),
                (SEI_CONTENT_LIGHT, CONTENT_LIGHT_PAYLOAD.to_vec()),
            ]
        );
    }
}

#[test]
fn content_light_level_is_big_endian_in_every_codec() {
    let light = ContentLightLevel {
        max_cll_nits: 0x1234,
        max_fall_nits: 0xabcd,
    };
    let hdr = HdrLevels {
        content_light: Some(light),
        ..HdrLevels::default()
    };

    let packed = sei(SeiNal::H264, &hdr).unwrap();
    let obus = metadata_obus(&hdr).unwrap();

    assert_eq!(
        packed.bytes,
        vec![0, 0, 0, 1, 0x06, 144, 4, 0x12, 0x34, 0xab, 0xcd, 0x80]
    );
    assert_eq!(
        obus.bytes,
        vec![AV1_METADATA_HEADER, 6, 1, 0x12, 0x34, 0xab, 0xcd, 0x80]
    );
}

#[test]
fn av1_metadata_obus_hold_fixed_point_payloads_in_spec_order() {
    for (mastering, content_light) in [(true, true), (true, false), (false, true)] {
        let packed = metadata_obus(&hdr(mastering, content_light)).unwrap();
        let mut expected = Vec::new();
        if mastering {
            expected.extend([AV1_METADATA_HEADER, 26, AV1_METADATA_MASTERING_DISPLAY]);
            expected.extend(MASTERING_AV1_PAYLOAD);
            expected.push(RBSP_STOP_BYTE);
        }
        if content_light {
            expected.extend([AV1_METADATA_HEADER, 6, AV1_METADATA_CONTENT_LIGHT]);
            expected.extend(CONTENT_LIGHT_PAYLOAD);
            expected.push(RBSP_STOP_BYTE);
        }

        assert_eq!(
            packed.bytes, expected,
            "mastering {mastering} light {content_light}"
        );
        assert_eq!(packed.bits, packed.bytes.len() * 8);
    }
}
