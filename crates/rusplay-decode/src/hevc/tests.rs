use std::io::Cursor;

use cros_codecs::codec::h265::parser::{NaluHeader, Parser};
use rusplay_core::Codec;

use super::*;
use crate::{annexb::next_nalu, fixtures as common};

const IDR_W_RADL: u8 = 19;
const TRAIL_R: u8 = 1;
const GENERAL_PROFILE_TIER_LEVEL_BITS: u32 = 96;
const POC_LSB_BITS: u32 = 4;
const MAX_DEC_PIC_BUFFERING_MINUS1: u64 = 16;
const CODING_BLOCK_SIZE: u64 = 16;
const SCALING_LISTS: usize = 20;
const FIXTURES: [&str; 4] = [
    "hevc-420-8bit.hevc",
    "hevc-420-10bit.hevc",
    "hevc-422-8bit.hevc",
    "hevc-420-8bit-bframes.hevc",
];

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

#[derive(Clone, Copy)]
enum ShortTermSet {
    Explicit(u64),
    PredictedWithOneMore,
}

struct ScreenContent {
    palette_max_size: u64,
    palette_delta: u64,
    initializers_minus1: u64,
}

struct Sequence {
    sub_layers: Vec<(bool, bool)>,
    width_in_ctbs: u64,
    height_in_ctbs: u64,
    scaling_first_delta: Option<u64>,
    short_term_sets: Vec<ShortTermSet>,
    long_term: Option<u64>,
    screen_content: Option<ScreenContent>,
}

impl Default for Sequence {
    fn default() -> Self {
        Self {
            sub_layers: Vec::new(),
            width_in_ctbs: 4,
            height_in_ctbs: 4,
            scaling_first_delta: None,
            short_term_sets: Vec::new(),
            long_term: None,
            screen_content: None,
        }
    }
}

impl Sequence {
    fn nalu(&self) -> Vec<u8> {
        let mut writer = Writer::default();
        writer
            .bits(4, 0)
            .bits(3, u64::try_from(self.sub_layers.len()).unwrap())
            .bit(true);

        writer.bits(GENERAL_PROFILE_TIER_LEVEL_BITS, 0);
        for &(profile, level) in &self.sub_layers {
            writer.bit(profile).bit(level);
        }
        if !self.sub_layers.is_empty() {
            writer.bits(2 * (8 - u32::try_from(self.sub_layers.len()).unwrap()), 0);
        }
        for &(_, level) in &self.sub_layers {
            if level {
                writer.bits(GENERAL_PROFILE_TIER_LEVEL_BITS, 0);
            }
        }

        writer.unsigned(0).unsigned(1);
        writer
            .unsigned(self.width_in_ctbs * CODING_BLOCK_SIZE)
            .unsigned(self.height_in_ctbs * CODING_BLOCK_SIZE);
        writer.bit(false);
        writer.unsigned(0).unsigned(0).unsigned(0);

        writer.bit(true);
        for _ in 0..=self.sub_layers.len() {
            writer
                .unsigned(MAX_DEC_PIC_BUFFERING_MINUS1)
                .unsigned(0)
                .unsigned(0);
        }

        writer.unsigned(1).unsigned(0).unsigned(0).unsigned(2);
        writer.unsigned(0).unsigned(0);

        writer.bit(self.scaling_first_delta.is_some());
        if let Some(delta) = self.scaling_first_delta {
            writer.bit(true).bit(false).unsigned(delta);
            for _ in 0..SCALING_LISTS - 1 {
                writer.bit(false).unsigned(0);
            }
        }

        writer.bit(false).bit(false).bit(false);
        self.write_short_term_sets(&mut writer);
        self.write_long_term(&mut writer);
        writer.bit(false).bit(false).bit(false);
        self.write_extension(&mut writer);

        writer.nalu(33)
    }

    fn write_short_term_sets(&self, writer: &mut Writer) {
        writer.unsigned(u64::try_from(self.short_term_sets.len()).unwrap());

        let mut pictures = 0_u64;
        for (index, set) in self.short_term_sets.iter().enumerate() {
            match *set {
                ShortTermSet::Explicit(count) => {
                    if index != 0 {
                        writer.bit(false);
                    }
                    writer.unsigned(count).unsigned(0);
                    for _ in 0..count {
                        writer.unsigned(0).bit(true);
                    }
                    pictures = count;
                }
                ShortTermSet::PredictedWithOneMore => {
                    writer.bit(true).bit(true).unsigned(0);
                    for _ in 0..=pictures {
                        writer.bit(true);
                    }
                    pictures += 1;
                }
            }
        }
    }

    fn write_long_term(&self, writer: &mut Writer) {
        writer.bit(self.long_term.is_some());
        if let Some(count) = self.long_term {
            writer.unsigned(count);
            for _ in 0..count {
                writer.bits(POC_LSB_BITS, 0).bit(true);
            }
        }
    }

    fn write_extension(&self, writer: &mut Writer) {
        let Some(content) = &self.screen_content else {
            writer.bit(false);
            return;
        };

        writer.bit(true).bit(false).bit(false).bit(false).bit(true);
        writer.bit(false).bit(true);
        writer
            .unsigned(content.palette_max_size)
            .unsigned(content.palette_delta);
        writer.bit(true).unsigned(content.initializers_minus1);
        for _ in 0..3 {
            for _ in 0..=content.initializers_minus1 {
                writer.bits(8, 0);
            }
        }
        writer.bits(2, 0).bit(false);
    }
}

enum TileSpacing {
    Uniform,
    Explicit {
        widths_minus1: Vec<u64>,
        heights_minus1: Vec<u64>,
    },
}

struct Tiles {
    columns_minus1: u64,
    rows_minus1: u64,
    spacing: TileSpacing,
}

#[derive(Default)]
struct Picture {
    tiles: Option<Tiles>,
    entropy_coding_sync: bool,
}

impl Picture {
    fn nalu(&self) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.unsigned(0).unsigned(0);
        writer
            .bit(false)
            .bit(false)
            .bits(3, 0)
            .bit(false)
            .bit(false);
        writer.unsigned(0).unsigned(0).unsigned(0);
        writer.bit(false).bit(false).bit(false);
        writer.unsigned(0).unsigned(0);
        writer.bit(false).bit(false).bit(false).bit(false);
        writer
            .bit(self.tiles.is_some())
            .bit(self.entropy_coding_sync);

        if let Some(tiles) = &self.tiles {
            writer
                .unsigned(tiles.columns_minus1)
                .unsigned(tiles.rows_minus1);
            match &tiles.spacing {
                TileSpacing::Uniform => {
                    writer.bit(true);
                }
                TileSpacing::Explicit {
                    widths_minus1,
                    heights_minus1,
                } => {
                    writer.bit(false);
                    for &width in widths_minus1.iter().chain(heights_minus1) {
                        writer.unsigned(width);
                    }
                }
            }
            writer.bit(true);
        }

        writer.bit(false).bit(false).bit(false).bit(false);
        writer.unsigned(0).bit(false).bit(false);

        writer.nalu(34)
    }
}

fn slice(kind: u8, body: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut writer = Writer::default();
    writer.bit(true);
    if (16..=23).contains(&kind) {
        writer.bit(false);
    }
    writer.unsigned(0).unsigned(2);
    body(&mut writer);
    writer.nalu(kind)
}

fn trailing_slice_with_explicit_set(rest: impl FnOnce(&mut Writer)) -> Vec<u8> {
    slice(TRAIL_R, |writer| {
        writer.bits(POC_LSB_BITS, 0).bit(false);
        writer.unsigned(0).unsigned(0);
        rest(writer);
    })
}

fn is_malformed<T>(result: &Result<T, DecodeError>, expected: BitstreamUnit) -> bool {
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

fn sequence_result(nalu: &[u8]) -> Result<bool, DecodeError> {
    check_sequence_parameter_set(nalu).map(|checked| checked.0.is_some())
}

fn picture_result(sequence: &Sequence, picture: &Picture) -> Result<(), DecodeError> {
    let mut sets = ParameterSets::default();
    let checked = check_sequence_parameter_set(&sequence.nalu())?;
    assert!(checked.0.is_some());
    sets.keep_sequence_parameter_set(checked);

    let checked = sets.check_picture_parameter_set(&picture.nalu())?;
    assert!(checked.0.is_some());

    Ok(())
}

fn slice_result(
    sequence: &Sequence,
    picture: &Picture,
    slice: &[u8],
    kind: u8,
) -> Result<(), DecodeError> {
    let mut sets = ParameterSets::default();
    let checked = check_sequence_parameter_set(&sequence.nalu())?;
    assert!(checked.0.is_some());
    sets.keep_sequence_parameter_set(checked);
    let checked = sets.check_picture_parameter_set(&picture.nalu())?;
    assert!(checked.0.is_some());
    sets.keep_picture_parameter_set(checked);

    sets.check_slice_header(slice, NaluType::try_from(u32::from(kind)).unwrap())
}

fn kept_sets(sequence: &Sequence, picture: &Picture) -> ParameterSets {
    let mut sets = ParameterSets::default();
    let checked = check_sequence_parameter_set(&sequence.nalu()).unwrap();
    assert!(checked.0.is_some());
    sets.keep_sequence_parameter_set(checked);
    let checked = sets.check_picture_parameter_set(&picture.nalu()).unwrap();
    assert!(checked.0.is_some());
    sets.keep_picture_parameter_set(checked);
    sets
}

fn tiled(columns_minus1: u64, rows_minus1: u64, spacing: TileSpacing) -> Picture {
    Picture {
        tiles: Some(Tiles {
            columns_minus1,
            rows_minus1,
            spacing,
        }),
        entropy_coding_sync: false,
    }
}

#[test]
fn the_fixture_parameter_sets_pass() {
    for file in FIXTURES {
        check_video_parameter_set(&parameter_set(file, 32)).unwrap();

        let mut sets = ParameterSets::default();
        let sequence = check_sequence_parameter_set(&parameter_set(file, 33)).unwrap();
        assert!(sequence.0.is_some(), "{file}");
        sets.keep_sequence_parameter_set(sequence);

        let picture = sets
            .check_picture_parameter_set(&parameter_set(file, 34))
            .unwrap();
        assert!(picture.0.is_some(), "{file}");
    }
}

#[test]
fn every_fixture_stream_passes_the_checks_the_parser_accepts() {
    for file in FIXTURES {
        let data = common::fixture(file);
        let mut cursor = Cursor::new(data.as_slice());
        let mut parser = Parser::default();
        let mut sets = ParameterSets::default();
        let mut slices = 0;

        while let Some(nalu) = next_nalu::<NaluHeader>(&mut cursor, Codec::Hevc).unwrap() {
            match nalu.header.type_ {
                NaluType::VpsNut => {
                    check_video_parameter_set(nalu.as_ref()).unwrap();
                    parser.parse_vps(&nalu).unwrap();
                }
                NaluType::SpsNut => {
                    let checked = check_sequence_parameter_set(nalu.as_ref()).unwrap();
                    parser.parse_sps(&nalu).unwrap();
                    assert!(checked.0.is_some(), "{file}");
                    sets.keep_sequence_parameter_set(checked);
                }
                NaluType::PpsNut => {
                    let checked = sets.check_picture_parameter_set(nalu.as_ref()).unwrap();
                    parser.parse_pps(&nalu).unwrap();
                    assert!(checked.0.is_some(), "{file}");
                    sets.keep_picture_parameter_set(checked);
                }
                kind if kind.is_irap() || (0..=9).contains(&(kind as u32)) => {
                    sets.check_slice_header(nalu.as_ref(), kind).unwrap();
                    parser.parse_slice_header(nalu).unwrap();
                    slices += 1;
                }
                _ => {}
            }
        }

        assert!(slices > 0, "{file}");
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
        &sequence_result(&nalu),
        BitstreamUnit::SequenceParameterSet
    ));
}

#[test]
fn a_scaling_list_predicted_from_a_missing_matrix_is_malformed() {
    let sequence = Sequence {
        scaling_first_delta: Some(1),
        ..Sequence::default()
    };

    assert!(is_malformed(
        &sequence_result(&sequence.nalu()),
        BitstreamUnit::SequenceParameterSet
    ));
}

#[test]
fn a_scaling_list_inferred_from_the_default_passes() {
    let sequence = Sequence {
        scaling_first_delta: Some(0),
        ..Sequence::default()
    };

    assert!(sequence_result(&sequence.nalu()).unwrap());
}

#[test]
fn a_scaling_list_after_sub_layer_levels_follows_the_parser_layout() {
    let sequence = Sequence {
        sub_layers: vec![(false, true), (true, false)],
        scaling_first_delta: Some(1),
        ..Sequence::default()
    };

    assert!(is_malformed(
        &sequence_result(&sequence.nalu()),
        BitstreamUnit::SequenceParameterSet
    ));
}

#[test]
fn a_set_predicted_from_sixty_four_pictures_is_malformed() {
    let mut short_term_sets = vec![ShortTermSet::Explicit(16)];
    short_term_sets.extend([ShortTermSet::PredictedWithOneMore; 48]);
    let reachable = Sequence {
        short_term_sets: short_term_sets.clone(),
        ..Sequence::default()
    };
    short_term_sets.push(ShortTermSet::PredictedWithOneMore);
    let unreachable = Sequence {
        short_term_sets,
        ..Sequence::default()
    };

    assert!(sequence_result(&reachable.nalu()).unwrap());
    assert!(is_malformed(
        &sequence_result(&unreachable.nalu()),
        BitstreamUnit::SequenceParameterSet
    ));
}

#[test]
fn a_palette_predictor_with_no_slots_and_initializers_is_malformed() {
    let sized = Sequence {
        screen_content: Some(ScreenContent {
            palette_max_size: 4,
            palette_delta: 4,
            initializers_minus1: 7,
        }),
        ..Sequence::default()
    };
    let empty = Sequence {
        screen_content: Some(ScreenContent {
            palette_max_size: 0,
            palette_delta: 0,
            initializers_minus1: 200,
        }),
        ..Sequence::default()
    };

    assert!(sequence_result(&sized.nalu()).unwrap());
    assert!(is_malformed(
        &sequence_result(&empty.nalu()),
        BitstreamUnit::SequenceParameterSet
    ));
}

#[test]
fn a_picture_parameter_set_without_its_sequence_is_left_to_the_parser() {
    let checked = ParameterSets::default()
        .check_picture_parameter_set(&Picture::default().nalu())
        .unwrap();

    assert!(checked.0.is_none());
}

#[test]
fn too_many_tile_columns_are_malformed() {
    let sequence = Sequence {
        width_in_ctbs: 40,
        ..Sequence::default()
    };

    picture_result(&sequence, &tiled(18, 0, TileSpacing::Uniform)).unwrap();
    assert!(is_malformed(
        &picture_result(&sequence, &tiled(19, 0, TileSpacing::Uniform)),
        BitstreamUnit::PictureParameterSet
    ));
}

#[test]
fn too_many_tile_rows_are_malformed() {
    let sequence = Sequence {
        height_in_ctbs: 40,
        ..Sequence::default()
    };

    picture_result(&sequence, &tiled(0, 20, TileSpacing::Uniform)).unwrap();
    assert!(is_malformed(
        &picture_result(&sequence, &tiled(0, 21, TileSpacing::Uniform)),
        BitstreamUnit::PictureParameterSet
    ));
}

#[test]
fn tile_columns_wider_than_the_picture_are_malformed() {
    let sequence = Sequence {
        width_in_ctbs: 3,
        ..Sequence::default()
    };
    let fits = TileSpacing::Explicit {
        widths_minus1: vec![0, 0],
        heights_minus1: vec![],
    };
    let overflows = TileSpacing::Explicit {
        widths_minus1: vec![1, 0],
        heights_minus1: vec![],
    };

    picture_result(&sequence, &tiled(2, 0, fits)).unwrap();
    assert!(is_malformed(
        &picture_result(&sequence, &tiled(2, 0, overflows)),
        BitstreamUnit::PictureParameterSet
    ));
}

#[test]
fn tile_rows_taller_than_the_picture_are_malformed() {
    let sequence = Sequence {
        height_in_ctbs: 3,
        ..Sequence::default()
    };
    let fits = TileSpacing::Explicit {
        widths_minus1: vec![],
        heights_minus1: vec![0, 0],
    };
    let overflows = TileSpacing::Explicit {
        widths_minus1: vec![],
        heights_minus1: vec![1, 0],
    };

    picture_result(&sequence, &tiled(0, 2, fits)).unwrap();
    assert!(is_malformed(
        &picture_result(&sequence, &tiled(0, 2, overflows)),
        BitstreamUnit::PictureParameterSet
    ));
}

#[test]
fn more_than_sixteen_long_term_pictures_are_malformed() {
    let sequence = Sequence {
        long_term: Some(0),
        ..Sequence::default()
    };
    let picture = Picture::default();

    let fits = trailing_slice_with_explicit_set(|writer| {
        writer.unsigned(16);
        for _ in 0..16 {
            writer.bits(POC_LSB_BITS, 0).bit(true).bit(false);
        }
    });
    let overflows = trailing_slice_with_explicit_set(|writer| {
        writer.unsigned(17);
    });

    slice_result(&sequence, &picture, &fits, TRAIL_R).unwrap();
    assert!(is_malformed(
        &slice_result(&sequence, &picture, &overflows, TRAIL_R),
        BitstreamUnit::SliceHeader
    ));
}

#[test]
fn long_term_pictures_from_the_sequence_count_against_the_limit() {
    let sequence = Sequence {
        long_term: Some(8),
        ..Sequence::default()
    };
    let picture = Picture::default();

    let overflows = trailing_slice_with_explicit_set(|writer| {
        writer.unsigned(8).unsigned(9);
    });

    assert!(is_malformed(
        &slice_result(&sequence, &picture, &overflows, TRAIL_R),
        BitstreamUnit::SliceHeader
    ));
}

#[test]
fn more_than_thirty_two_entry_points_are_malformed() {
    let sequence = Sequence {
        height_in_ctbs: 40,
        ..Sequence::default()
    };
    let picture = Picture {
        tiles: None,
        entropy_coding_sync: true,
    };

    let with_entry_points = |entry_points| {
        slice(IDR_W_RADL, |writer| {
            writer.unsigned(0);
            writer.unsigned(entry_points);
        })
    };

    slice_result(&sequence, &picture, &with_entry_points(32), IDR_W_RADL).unwrap();
    assert!(is_malformed(
        &slice_result(&sequence, &picture, &with_entry_points(33), IDR_W_RADL),
        BitstreamUnit::SliceHeader
    ));
}

#[test]
fn a_slice_of_an_unknown_picture_parameter_set_is_malformed() {
    let nalu = slice(IDR_W_RADL, |_| {});

    let result =
        ParameterSets::default().check_slice_header(&nalu, NaluType::try_from(19).unwrap());

    assert!(is_malformed(&result, BitstreamUnit::SliceHeader));
}

#[test]
fn a_rejected_parameter_set_leaves_the_kept_one_in_place() {
    let sequence = Sequence::default();
    let picture = Picture::default();
    let sets = kept_sets(&sequence, &picture);

    let nalu = slice(IDR_W_RADL, |writer| {
        writer.unsigned(0);
    });

    sets.check_slice_header(&nalu, NaluType::try_from(19).unwrap())
        .unwrap();
}

#[test]
fn every_truncation_of_a_parameter_set_is_handled() {
    let sps = parameter_set("hevc-422-8bit.hevc", 33);
    let pps = parameter_set("hevc-422-8bit.hevc", 34);
    let mut sets = ParameterSets::default();
    let checked = check_sequence_parameter_set(&sps).unwrap();
    sets.keep_sequence_parameter_set(checked);

    for length in 0..sps.len() {
        let _ = sequence_result(&sps[..length]);
    }
    for length in 0..pps.len() {
        let _ = sets.check_picture_parameter_set(&pps[..length]);
    }
}

#[test]
fn every_truncation_of_a_slice_header_is_handled() {
    for file in FIXTURES {
        let data = common::fixture(file);
        let mut cursor = Cursor::new(data.as_slice());
        let mut sets = ParameterSets::default();

        while let Some(nalu) = next_nalu::<NaluHeader>(&mut cursor, Codec::Hevc).unwrap() {
            match nalu.header.type_ {
                NaluType::SpsNut => {
                    let checked = check_sequence_parameter_set(nalu.as_ref()).unwrap();
                    sets.keep_sequence_parameter_set(checked);
                }
                NaluType::PpsNut => {
                    let checked = sets.check_picture_parameter_set(nalu.as_ref()).unwrap();
                    sets.keep_picture_parameter_set(checked);
                }
                kind if kind.is_irap() || (0..=9).contains(&(kind as u32)) => {
                    let bytes = nalu.as_ref();
                    for length in 0..bytes.len().min(64) {
                        let _ = sets.check_slice_header(&bytes[..length], kind);
                    }
                }
                _ => {}
            }
        }
    }
}
