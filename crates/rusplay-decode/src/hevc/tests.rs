use super::*;
use crate::fixtures as common;

#[derive(Default)]
struct Writer {
    bits: Vec<bool>,
}

impl Writer {
    fn bit(&mut self, value: bool) -> &mut Self {
        self.bits.push(value);
        self
    }

    fn bits(&mut self, count: u32, value: u64) -> &mut Self {
        for shift in (0..count).rev() {
            self.bits
                .push(value.checked_shr(shift).unwrap_or(0) & 1 == 1);
        }
        self
    }

    fn unsigned(&mut self, value: u64) -> &mut Self {
        let coded = value + 1;
        let length = coded.bit_width();
        self.bits(length - 1, 0).bits(length, coded)
    }

    fn nalu(&self, kind: u8) -> Vec<u8> {
        let mut bits = self.bits.clone();
        bits.push(true);
        while !bits.len().is_multiple_of(8) {
            bits.push(false);
        }

        let mut nalu = vec![kind << 1, 1];
        nalu.extend(bits.chunks(8).map(|byte| {
            byte.iter()
                .fold(0u8, |value, &bit| (value << 1) | u8::from(bit))
        }));
        nalu
    }
}

fn is_malformed(result: &Result<(), DecodeError>, expected: BitstreamUnit) -> bool {
    matches!(
        result,
        Err(DecodeError::Malformed {
            codec: Codec::Hevc,
            unit,
            ..
        }) if *unit == expected
    )
}

fn parameter_set(file: &str, kind: u8) -> Vec<u8> {
    let data = common::fixture(file);
    let start = data
        .windows(4)
        .position(|window| window[..3] == [0, 0, 1] && (window[3] >> 1) & 0x3f == kind)
        .unwrap()
        + 3;
    let end = data[start..]
        .windows(3)
        .position(|window| window == [0, 0, 1])
        .map_or(data.len(), |offset| start + offset);

    let mut nalu = data[start..end].to_vec();
    while nalu.last() == Some(&0) {
        nalu.pop();
    }
    nalu
}

fn sequence_parameter_set_with_scaling(first_delta: u64) -> Vec<u8> {
    let mut writer = Writer::default();
    writer.bits(4, 0).bits(3, 0).bit(true);
    writer.bits(96, 0);
    writer.unsigned(0).unsigned(1).unsigned(64).unsigned(64);
    writer.bit(false);
    writer.unsigned(0).unsigned(0).unsigned(0);
    writer.bit(true).unsigned(1).unsigned(0).unsigned(0);
    for _ in 0..6 {
        writer.unsigned(0);
    }
    writer.bit(true).bit(true);
    writer.bit(false).unsigned(first_delta);
    writer.nalu(33)
}

#[test]
fn the_fixture_parameter_sets_pass() {
    for file in [
        "hevc-420-8bit.hevc",
        "hevc-420-10bit.hevc",
        "hevc-422-8bit.hevc",
    ] {
        check_video_parameter_set(&parameter_set(file, 32)).unwrap();
        check_sequence_parameter_set(&parameter_set(file, 33)).unwrap();
        check_picture_parameter_set(&parameter_set(file, 34)).unwrap();
    }
}

#[test]
fn seven_sub_layers_in_a_video_parameter_set_are_malformed() {
    let mut nalu = parameter_set("hevc-420-8bit.hevc", 32);
    nalu[3] |= 0b0000_1110;

    assert!(is_malformed(
        &check_video_parameter_set(&nalu),
        BitstreamUnit::VideoParameterSet
    ));
}

#[test]
fn seven_sub_layers_in_a_sequence_parameter_set_are_malformed() {
    let mut nalu = parameter_set("hevc-420-8bit.hevc", 33);
    nalu[2] |= 0b0000_1110;

    assert!(is_malformed(
        &check_sequence_parameter_set(&nalu),
        BitstreamUnit::SequenceParameterSet
    ));
}

#[test]
fn a_scaling_list_predicted_from_a_missing_matrix_is_malformed() {
    let nalu = sequence_parameter_set_with_scaling(1);

    assert!(is_malformed(
        &check_sequence_parameter_set(&nalu),
        BitstreamUnit::SequenceParameterSet
    ));
}

#[test]
fn a_scaling_list_inferred_from_the_default_passes() {
    check_sequence_parameter_set(&sequence_parameter_set_with_scaling(0)).unwrap();
}

#[test]
fn every_truncation_of_a_parameter_set_is_handled() {
    let sps = parameter_set("hevc-422-8bit.hevc", 33);
    let pps = parameter_set("hevc-422-8bit.hevc", 34);

    for length in 0..sps.len() {
        let _ = check_sequence_parameter_set(&sps[..length]);
    }
    for length in 0..pps.len() {
        let _ = check_picture_parameter_set(&pps[..length]);
    }
}
