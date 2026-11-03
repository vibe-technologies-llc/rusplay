use cros_codecs::codec::h265::parser::NaluHeader;

use super::*;

#[test]
fn a_nal_unit_shorter_than_its_header_is_malformed() {
    let data = [0, 0, 1, 0x40];
    let mut cursor = Cursor::new(&data[..]);

    let result = next_nalu::<NaluHeader>(&mut cursor, Codec::Hevc);

    assert!(matches!(
        result,
        Err(DecodeError::Malformed {
            codec: Codec::Hevc,
            unit: BitstreamUnit::NalUnit,
            ..
        })
    ));
}

#[test]
fn a_nal_unit_with_a_payload_passes() {
    let data = [0, 0, 1, 0x40, 0x01, 0xaa];
    let mut cursor = Cursor::new(&data[..]);

    let nalu = next_nalu::<NaluHeader>(&mut cursor, Codec::Hevc).unwrap();

    assert!(nalu.is_some());
}
