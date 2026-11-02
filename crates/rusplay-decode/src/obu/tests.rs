use super::*;
use crate::fixtures as common;

fn is_malformed(result: &Result<(), DecodeError>) -> bool {
    matches!(
        result,
        Err(DecodeError::Malformed {
            codec: Codec::Av1,
            unit: BitstreamUnit::Obu,
            ..
        })
    )
}

#[test]
fn every_fixture_packet_passes() {
    let frames = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit-altref.ivf"));

    let first = frames.first().unwrap();

    check_framing(&first.data).unwrap();
    for frame in &frames {
        check_framing(&frame.data).unwrap();
    }
}

#[test]
fn an_empty_packet_passes() {
    check_framing(&[]).unwrap();
}

#[test]
fn an_obu_without_a_size_field_is_malformed() {
    assert!(is_malformed(&check_framing(&[0x10, 0x00])));
}

#[test]
fn a_set_reserved_bit_is_malformed() {
    assert!(is_malformed(&check_framing(&[0x13, 0x00])));
}

#[test]
fn a_size_past_the_packet_end_is_malformed() {
    assert!(is_malformed(&check_framing(&[0x12, 0x05, 0x00])));
    assert!(is_malformed(&check_framing(&[0x32, 0x02, 0xaa])));
}

#[test]
fn a_huge_leb128_size_is_malformed() {
    let packet = [0x32, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0x00];

    assert!(is_malformed(&check_framing(&packet)));
}

#[test]
fn an_unterminated_or_truncated_size_is_malformed() {
    let unterminated = [0x32, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01];

    assert!(is_malformed(&check_framing(&unterminated)));
    assert!(is_malformed(&check_framing(&[0x32, 0x80])));
    assert!(is_malformed(&check_framing(&[0x32])));
}

#[test]
fn a_truncated_extension_header_is_malformed() {
    assert!(is_malformed(&check_framing(&[0x16])));
    assert!(is_malformed(&check_framing(&[0x16, 0x00])));
}

#[test]
fn consecutive_obus_with_extension_headers_pass() {
    let packet = [0x12, 0x00, 0x16, 0x08, 0x01, 0xaa, 0x32, 0x81, 0x00, 0xbb];

    check_framing(&packet).unwrap();
}

#[test]
fn every_truncation_of_a_fixture_packet_is_handled() {
    let frames = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit-altref.ivf"));
    let packet = &frames[0].data;

    for length in 0..packet.len() {
        let _ = check_framing(&packet[..length]);
    }
}
