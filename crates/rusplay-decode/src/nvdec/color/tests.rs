use rusplay_core::{ColorPrimaries, ColorRange, Luminance};

use super::*;
use crate::fixtures as common;

fn read_all(codec: Codec, frames: &[rusplay_core::EncodedFrame]) -> Vec<SignalledColor> {
    let mut reader = ColorReader::new(codec);

    frames
        .iter()
        .map(|frame| {
            reader.read(&frame.data).unwrap();
            reader.current()
        })
        .collect()
}

#[test]
fn h264_without_a_video_signal_type_signals_nothing() {
    let frames =
        common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit-bframes.h264"));

    let colors = read_all(Codec::H264, &frames);

    assert_eq!(colors.len(), 12);
    assert!(
        colors
            .iter()
            .all(|color| *color == SignalledColor::default())
    );
}

#[test]
fn hevc_and_av1_signal_their_limited_range() {
    let hevc = common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-10bit.hevc"));
    let av1 = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit-altref.ivf"));
    let limited = SignalledColor {
        range: Some(ColorRange::Limited),
        ..SignalledColor::default()
    };

    let hevc_colors = read_all(Codec::Hevc, &hevc);
    let av1_colors = read_all(Codec::Av1, &av1);

    assert!(hevc_colors.iter().all(|color| *color == limited));
    assert!(av1_colors.iter().all(|color| *color == limited));
}

#[test]
fn hevc_mastering_sei_holds_until_the_next_irap() {
    let frames = common::annexb_access_units(Codec::Hevc, &common::fixture("hevc-420-8bit.hevc"));
    let mut sei = vec![0, 0, 0, 1, 0x4e, 0x01, 137, 24];
    for value in [
        8_500_u16, 39_850, 6_550, 2_300, 35_400, 14_600, 15_635, 16_450,
    ] {
        sei.extend_from_slice(&value.to_be_bytes());
    }
    sei.extend_from_slice(&10_000_000_u32.to_be_bytes());
    sei.extend_from_slice(&50_u32.to_be_bytes());
    sei.push(0x80);
    let mut keyframe = frames[0].data.clone();
    keyframe.extend_from_slice(&sei);
    let mut reader = ColorReader::new(Codec::Hevc);

    reader.read(&keyframe).unwrap();
    let first = reader.current();
    reader.read(&frames[1].data).unwrap();
    let second = reader.current();
    reader.read(&frames[0].data).unwrap();
    let next_irap = reader.current();

    let mastering = first.mastering.unwrap();
    assert_eq!(mastering.primaries, ColorPrimaries::Bt2020);
    assert_eq!(
        mastering.max_nits,
        Luminance::from_whole_nits(1000).unwrap()
    );
    assert_eq!(second.mastering, first.mastering);
    assert_eq!(next_irap.mastering, None);
}

#[test]
fn a_slice_before_its_parameter_sets_is_reported() {
    let frames = common::annexb_access_units(Codec::H264, &common::fixture("h264-420-8bit.h264"));
    let mut reader = ColorReader::new(Codec::H264);

    let result = reader.read(&frames[1].data);

    assert!(matches!(
        result,
        Err(DecodeError::Malformed {
            codec: Codec::H264,
            unit: BitstreamUnit::SliceHeader,
            ..
        })
    ));
}
