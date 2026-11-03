use rusplay_core::Codec;

use super::*;
use crate::fixtures as common;

pub(crate) const SEQUENCE_HEADER: u8 = 1;
pub(crate) const TEMPORAL_DELIMITER: u8 = 2;
pub(crate) const FRAME_HEADER: u8 = 3;
pub(crate) const TILE_GROUP: u8 = 4;
pub(crate) const FRAME: u8 = 6;
const REDUNDANT_FRAME_HEADER: u8 = 7;
const BASE_Q_INDEX: u64 = 100;

#[derive(Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    pub(crate) fn put(&mut self, value: u64, count: u32) {
        for shift in (0..count).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            let bit = u8::try_from((value >> shift) & 1).unwrap();
            let last = self.bytes.last_mut().unwrap();
            *last |= bit << (7 - self.bits % 8);
            self.bits += 1;
        }
    }

    pub(crate) fn flag(&mut self, value: bool) {
        self.put(u64::from(value), 1);
    }

    pub(crate) fn zeros(&mut self, count: u32) {
        for _ in 0..count {
            self.flag(false);
        }
    }

    fn aligned(self) -> Vec<u8> {
        self.bytes
    }

    fn trailing(mut self) -> Vec<u8> {
        self.flag(true);
        self.bytes
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Shape {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) superres: bool,
    pub(crate) operating_point_idc: u64,
}

impl Shape {
    pub(crate) const fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            superres: false,
            operating_point_idc: 0,
        }
    }
}

pub(crate) fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![(kind << 3) | 0x02];
    let mut size = payload.len();
    loop {
        let byte = u8::try_from(size & 0x7f).unwrap();
        size >>= 7;
        if size == 0 {
            bytes.push(byte);
            break;
        }
        bytes.push(byte | 0x80);
    }
    bytes.extend_from_slice(payload);
    bytes
}

pub(crate) fn sequence_header(shape: Shape) -> Vec<u8> {
    let mut w = BitWriter::default();

    w.put(0, 3);
    w.flag(false);
    w.flag(false);
    w.flag(false);
    w.flag(false);
    w.put(0, 5);
    w.put(shape.operating_point_idc, 12);
    w.put(0, 5);

    w.put(15, 4);
    w.put(15, 4);
    w.put(u64::from(shape.width - 1), 16);
    w.put(u64::from(shape.height - 1), 16);

    w.zeros(4);
    w.zeros(5);
    w.flag(false);
    w.flag(false);

    w.flag(shape.superres);
    w.flag(false);
    w.flag(false);

    w.zeros(4);
    w.put(0, 2);
    w.flag(false);
    w.flag(false);

    w.trailing()
}

fn key_frame_bits(shape: Shape, tile_info: impl FnOnce(&mut BitWriter)) -> BitWriter {
    let mut w = BitWriter::default();

    w.flag(false);
    w.put(0, 2);
    w.flag(true);
    w.flag(false);
    w.flag(false);
    if shape.superres {
        w.flag(false);
    }
    w.flag(false);
    w.flag(false);

    tile_info(&mut w);

    w.put(BASE_Q_INDEX, 8);
    w.zeros(4);
    w.flag(false);
    w.flag(false);
    w.zeros(15);
    w.flag(false);
    w.flag(false);
    w.flag(false);

    w
}

pub(crate) fn key_frame_header(shape: Shape, tile_info: impl FnOnce(&mut BitWriter)) -> Vec<u8> {
    key_frame_bits(shape, tile_info).trailing()
}

fn one_tile(w: &mut BitWriter) {
    w.flag(true);
}

pub(crate) fn two_columns(w: &mut BitWriter) {
    w.flag(true);
    w.flag(true);
    w.put(0, 1);
    w.put(0, 2);
}

pub(crate) fn explicit_columns(columns: u32) -> impl FnOnce(&mut BitWriter) {
    move |w| {
        w.flag(false);
        w.zeros(7 * columns);
    }
}

pub(crate) fn tile_group(header: impl FnOnce(&mut BitWriter), tiles: &[&[u8]]) -> Vec<u8> {
    let mut w = BitWriter::default();
    header(&mut w);
    let mut bytes = w.aligned();

    for (index, tile) in tiles.iter().enumerate() {
        if index + 1 < tiles.len() {
            bytes.push(u8::try_from(tile.len() - 1).unwrap());
        }
        bytes.extend_from_slice(tile);
    }
    bytes
}

pub(crate) fn whole_frame(w: &mut BitWriter) {
    w.flag(false);
}

pub(crate) fn packet(units: &[(u8, Vec<u8>)]) -> Vec<u8> {
    units
        .iter()
        .flat_map(|(kind, payload)| obu(*kind, payload))
        .collect()
}

fn started(shape: Shape) -> Av1Syntax {
    let mut syntax = Av1Syntax::default();
    syntax
        .check_packet(&packet(&[(SEQUENCE_HEADER, sequence_header(shape))]))
        .unwrap();
    syntax
}

fn rejected(result: Result<PacketSummary, DecodeError>) -> Av1SyntaxError {
    match result {
        Err(DecodeError::Av1Syntax(error)) => error,
        other => panic!("expected an av1 syntax error, got {other:?}"),
    }
}

fn is_malformed(result: &Result<Vec<Unit<'_>>, DecodeError>) -> bool {
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
fn every_fixture_passes_in_order() {
    for name in [
        "av1-420-8bit.ivf",
        "av1-420-10bit.ivf",
        "av1-444-8bit.ivf",
        "av1-420-8bit-altref.ivf",
    ] {
        let mut syntax = Av1Syntax::default();

        for frame in common::ivf_frames(Codec::Av1, &common::fixture(name)) {
            syntax
                .check_packet(&frame.data)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }

        assert!(syntax.frame.is_none(), "{name} ends inside a frame");
    }
}

#[test]
fn the_tiled_fixture_has_four_tiles() {
    let frames = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit-altref.ivf"));
    let units = framing::units(&frames[0].data).unwrap();

    let sequence = units
        .iter()
        .find(|unit| unit.kind == ObuKind::SequenceHeader)
        .unwrap();
    let mut syntax = Av1Syntax::default();
    syntax.obu(sequence).unwrap();

    let frame = units
        .iter()
        .find(|unit| matches!(unit.kind, ObuKind::Frame | ObuKind::FrameHeader))
        .unwrap();
    let parsed = frame::parse(
        &syntax.sequence.as_ref().unwrap().header,
        &syntax.slots,
        frame,
        &mut BitReader::new(frame.payload, BitstreamUnit::FrameHeader),
    )
    .unwrap();

    let Header::Frame(header) = parsed else {
        panic!("the first frame is shown from a slot");
    };
    assert_eq!((header.layout.cols, header.layout.rows), (2, 2));
}

#[test]
fn a_valid_two_tile_frame_completes_and_refreshes_every_slot() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let data = packet(&[
        (TEMPORAL_DELIMITER, Vec::new()),
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TILE_GROUP, tile_group(whole_frame, &[&[1, 2], &[3, 4, 5]])),
    ]);
    syntax.check_packet(&data).unwrap();

    assert!(syntax.frame.is_none());
    assert!(syntax.slots.iter().all(|slot| {
        slot.frame.is_some_and(|frame| {
            frame.key && frame.upscaled_width == 128 && frame.frame_height == 64
        })
    }));
}

#[test]
fn a_frame_obu_carries_its_tile_group_after_the_header() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let mut payload = key_frame_bits(shape, two_columns).aligned();
    payload.extend(tile_group(whole_frame, &[&[9], &[8, 7]]));
    syntax.check_packet(&packet(&[(FRAME, payload)])).unwrap();

    assert!(syntax.frame.is_none());
    assert!(syntax.slots[0].frame.is_some());
}

#[test]
fn tile_groups_may_split_a_frame_across_packets() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let first = |w: &mut BitWriter| {
        w.flag(true);
        w.put(0, 1);
        w.put(0, 1);
    };
    let second = |w: &mut BitWriter| {
        w.flag(true);
        w.put(1, 1);
        w.put(1, 1);
    };

    syntax
        .check_packet(&packet(&[
            (FRAME_HEADER, key_frame_header(shape, two_columns)),
            (TILE_GROUP, tile_group(first, &[&[1]])),
        ]))
        .unwrap();
    assert!(syntax.frame.is_some());

    syntax
        .check_packet(&packet(&[(TILE_GROUP, tile_group(second, &[&[2]]))]))
        .unwrap();
    assert!(syntax.frame.is_none());
}

#[test]
fn sixty_four_explicit_tile_columns_pass() {
    let shape = Shape::new(4096, 64);
    let mut syntax = started(shape);

    let tile_info = |w: &mut BitWriter| {
        w.flag(false);
        for remaining in (1..=64u32).rev() {
            w.zeros(remaining.ilog2());
        }
        w.put(0, 6);
        w.put(0, 2);
    };
    let tiles: Vec<[u8; 1]> = (0..64).map(|index| [index]).collect();
    let tiles: Vec<&[u8]> = tiles.iter().map(<[u8; 1]>::as_slice).collect();

    syntax
        .check_packet(&packet(&[
            (FRAME_HEADER, key_frame_header(shape, tile_info)),
            (TILE_GROUP, tile_group(whole_frame, &tiles)),
        ]))
        .unwrap();
}

#[test]
fn sixty_five_tile_columns_are_rejected() {
    let shape = Shape::new(4160, 64);
    let mut syntax = started(shape);

    let data = packet(&[(FRAME_HEADER, key_frame_header(shape, explicit_columns(65)))]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::TileColumnsOutOfRange(65)
    );
}

#[test]
fn sixty_five_tile_rows_are_rejected() {
    let shape = Shape::new(64, 4160);
    let mut syntax = started(shape);

    let data = packet(&[(FRAME_HEADER, key_frame_header(shape, explicit_columns(65)))]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::TileRowsOutOfRange(65)
    );
}

#[test]
fn a_context_update_tile_past_the_tile_count_is_rejected() {
    let shape = Shape::new(192, 64);
    let mut syntax = started(shape);

    let tile_info = |w: &mut BitWriter| {
        w.flag(false);
        w.put(0, 1);
        w.put(0, 1);
        w.put(3, 2);
    };
    let data = packet(&[(FRAME_HEADER, key_frame_header(shape, tile_info))]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::ContextUpdateTileOutOfRange { tile: 3, tiles: 3 }
    );
}

#[test]
fn a_tile_size_past_the_tile_group_is_rejected() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let mut tiles = tile_group(whole_frame, &[]);
    tiles.extend([0xff, 1, 2, 3]);
    let data = packet(&[
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TILE_GROUP, tiles),
    ]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::TileSizePastData {
            tile: 0,
            size: 256,
            remaining: 3
        }
    );
}

#[test]
fn a_missing_tile_size_field_is_truncated() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let data = packet(&[
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TILE_GROUP, tile_group(whole_frame, &[])),
    ]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::Truncated {
            unit: BitstreamUnit::TileGroup
        }
    );
}

#[test]
fn an_empty_last_tile_is_rejected() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let data = packet(&[
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TILE_GROUP, tile_group(whole_frame, &[&[1, 2], &[]])),
    ]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::EmptyTile { tile: 1 }
    );
}

#[test]
fn a_single_tile_frame_without_data_is_rejected() {
    let shape = Shape::new(64, 64);
    let mut syntax = started(shape);

    let data = packet(&[
        (FRAME_HEADER, key_frame_header(shape, one_tile)),
        (TILE_GROUP, Vec::new()),
    ]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::EmptyTile { tile: 0 }
    );
}

#[test]
fn a_tile_group_that_skips_ahead_is_rejected() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let skipping = |w: &mut BitWriter| {
        w.flag(true);
        w.put(1, 1);
        w.put(1, 1);
    };
    let data = packet(&[
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TILE_GROUP, tile_group(skipping, &[&[1]])),
    ]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::TileGroupOutOfOrder {
            start: 1,
            end: 1,
            expected: 0,
            tiles: 2
        }
    );
}

#[test]
fn a_tile_group_without_a_frame_header_is_rejected() {
    let mut syntax = started(Shape::new(64, 64));

    let data = packet(&[(TILE_GROUP, tile_group(whole_frame, &[&[1]]))]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::TileGroupWithoutFrameHeader
    );
}

#[test]
fn a_frame_header_before_the_sequence_header_is_rejected() {
    let mut syntax = Av1Syntax::default();

    let data = packet(&[(FRAME_HEADER, key_frame_header(Shape::new(64, 64), one_tile))]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::FrameBeforeSequenceHeader
    );
}

#[test]
fn a_truncated_frame_header_is_rejected() {
    let shape = Shape::new(64, 64);
    let mut syntax = started(shape);

    let mut header = key_frame_header(shape, one_tile);
    header.truncate(2);
    let data = packet(&[(FRAME_HEADER, header)]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::Truncated {
            unit: BitstreamUnit::FrameHeader
        }
    );
}

#[test]
fn an_inter_frame_sized_from_an_empty_slot_is_rejected() {
    let shape = Shape::new(64, 64);
    let mut syntax = started(shape);

    let mut w = BitWriter::default();
    w.flag(false);
    w.put(1, 2);
    w.flag(true);
    w.flag(false);
    w.flag(false);
    w.flag(true);
    w.put(7, 3);
    w.put(1, 8);
    for slot in 0..7 {
        w.put(slot, 3);
    }
    w.flag(true);
    let data = packet(&[(FRAME_HEADER, w.trailing())]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::EmptyReferenceSlot { slot: 0 }
    );
}

#[test]
fn showing_an_empty_slot_is_rejected() {
    let mut syntax = started(Shape::new(64, 64));

    let mut w = BitWriter::default();
    w.flag(true);
    w.put(5, 3);
    let data = packet(&[(FRAME_HEADER, w.trailing())]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::EmptyReferenceSlot { slot: 5 }
    );
}

#[test]
fn superres_that_does_not_downscale_is_rejected() {
    let shape = Shape {
        width: 2,
        height: 2,
        superres: true,
        operating_point_idc: 0,
    };
    let mut syntax = started(shape);

    let mut w = BitWriter::default();
    w.flag(false);
    w.put(0, 2);
    w.flag(true);
    w.flag(false);
    w.flag(false);
    w.flag(true);
    w.put(0, 3);
    let data = packet(&[(FRAME_HEADER, w.trailing())]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::SuperresWithoutDownscale { width: 2 }
    );
}

#[test]
fn a_second_frame_header_inside_a_frame_is_rejected_unless_it_repeats_the_first() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let header = key_frame_header(shape, two_columns);
    syntax
        .check_packet(&packet(&[(FRAME_HEADER, header.clone())]))
        .unwrap();

    syntax
        .check_packet(&packet(&[
            (FRAME_HEADER, header),
            (REDUNDANT_FRAME_HEADER, vec![0xff]),
        ]))
        .unwrap();

    let other = key_frame_header(Shape::new(128, 64), one_tile);
    assert_eq!(
        rejected(syntax.check_packet(&packet(&[(FRAME_HEADER, other)]))),
        Av1SyntaxError::FrameHeaderDuringFrame
    );
}

#[test]
fn a_redundant_frame_header_without_its_frame_is_rejected() {
    let mut syntax = started(Shape::new(64, 64));

    let data = packet(&[(REDUNDANT_FRAME_HEADER, vec![0x10])]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::RedundantFrameHeaderWithoutFrame
    );
}

#[test]
fn a_temporal_delimiter_abandons_an_unfinished_frame() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let data = packet(&[
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TEMPORAL_DELIMITER, Vec::new()),
        (TILE_GROUP, tile_group(whole_frame, &[&[1], &[2]])),
    ]);

    assert_eq!(
        rejected(syntax.check_packet(&data)),
        Av1SyntaxError::TileGroupWithoutFrameHeader
    );
}

#[test]
fn a_rejected_packet_leaves_the_state_untouched() {
    let shape = Shape::new(128, 64);
    let mut syntax = started(shape);

    let mut bad = tile_group(whole_frame, &[]);
    bad.push(0xff);
    let data = packet(&[
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TILE_GROUP, bad),
    ]);
    assert!(syntax.check_packet(&data).is_err());

    assert!(syntax.frame.is_none());
    assert!(syntax.slots.iter().all(|slot| slot.frame.is_none()));
}

#[test]
fn a_decode_error_forgets_the_references() {
    let shape = Shape::new(64, 64);
    let mut syntax = started(shape);

    let data = packet(&[
        (FRAME_HEADER, key_frame_header(shape, one_tile)),
        (TILE_GROUP, vec![1]),
    ]);
    let summary = syntax.check_packet(&data).unwrap();
    assert!(syntax.slots[0].frame.is_some());

    syntax.forget_after_decode_error(summary);

    assert!(syntax.slots.iter().all(|slot| slot.frame.is_none()));
    assert!(syntax.sequence.is_some());
}

#[test]
fn an_empty_packet_passes() {
    framing::units(&[]).unwrap();
}

#[test]
fn an_obu_without_a_size_field_is_malformed() {
    assert!(is_malformed(&framing::units(&[0x10, 0x00])));
}

#[test]
fn a_set_reserved_bit_is_malformed() {
    assert!(is_malformed(&framing::units(&[0x13, 0x00])));
}

#[test]
fn a_size_past_the_packet_end_is_malformed() {
    assert!(is_malformed(&framing::units(&[0x12, 0x05, 0x00])));
    assert!(is_malformed(&framing::units(&[0x32, 0x02, 0xaa])));
}

#[test]
fn a_huge_leb128_size_is_malformed() {
    let packet = [0x32, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0x00];

    assert!(is_malformed(&framing::units(&packet)));
}

#[test]
fn an_unterminated_or_truncated_size_is_malformed() {
    let unterminated = [0x32, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01];

    assert!(is_malformed(&framing::units(&unterminated)));
    assert!(is_malformed(&framing::units(&[0x32, 0x80])));
    assert!(is_malformed(&framing::units(&[0x32])));
}

#[test]
fn a_truncated_extension_header_is_malformed() {
    assert!(is_malformed(&framing::units(&[0x16])));
    assert!(is_malformed(&framing::units(&[0x16, 0x00])));
}

#[test]
fn consecutive_obus_with_extension_headers_pass() {
    let packet = [0x12, 0x00, 0x16, 0x08, 0x01, 0xaa, 0x32, 0x81, 0x00, 0xbb];

    let units = framing::units(&packet).unwrap();

    assert_eq!(units.len(), 3);
    assert_eq!(
        units[1].layer,
        Some(framing::Layer {
            temporal_id: 0,
            spatial_id: 1
        })
    );
}

#[test]
fn every_truncation_of_a_fixture_packet_is_handled() {
    let frames = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit-altref.ivf"));
    let packet = &frames[0].data;

    for length in 0..packet.len() {
        let mut syntax = Av1Syntax::default();
        let _ = syntax.check_packet(&packet[..length]);
    }
}
