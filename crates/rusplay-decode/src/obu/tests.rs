use cros_codecs::codec::av1::parser::ObuAction;

use super::*;
use crate::{
    av1::tests::{
        FRAME_HEADER, SEQUENCE_HEADER, Shape, TILE_GROUP, explicit_columns, key_frame_header,
        packet, sequence_header, tile_group, two_columns, whole_frame,
    },
    fixtures as common,
};

fn feed(parser: &mut ObuParser, mut data: &[u8]) -> Result<(), DecodeError> {
    while !data.is_empty() {
        let obu = match parser.read_obu(data)? {
            ObuAction::Process(obu) => obu,
            ObuAction::Drop(length) => {
                data = &data[usize::try_from(length).unwrap()..];
                continue;
            }
        };
        let length = obu.bytes_used;
        match parser.parse(obu)? {
            ParsedObu::FrameHeader(header) => parser.ref_frame_update(&header)?,
            ParsedObu::Frame(frame) => parser.ref_frame_update(&frame.header)?,
            _ => {}
        }
        data = &data[length..];
    }
    Ok(())
}

#[test]
fn a_tile_group_before_any_frame_header_is_malformed() {
    let packet = [0x22, 0x01, 0x00];
    let mut parser = ObuParser::new();

    let ObuAction::Process(obu) = parser.read_obu(&packet).unwrap() else {
        panic!("the tile group was dropped");
    };
    let result = parser.parse(obu);

    assert!(matches!(
        result,
        Err(DecodeError::Av1Syntax(
            Av1SyntaxError::TileGroupWithoutFrameHeader
        ))
    ));
}

#[test]
fn an_inter_frame_before_any_key_frame_is_malformed() {
    let frames = common::ivf_frames(Codec::Av1, &common::fixture("av1-420-8bit.ivf"));
    let mut parser = ObuParser::new();

    let mut data = frames[0].data.as_slice();
    let mut sequence_parsed = false;
    while !sequence_parsed {
        let ObuAction::Process(obu) = parser.read_obu(data).unwrap() else {
            panic!("an obu was dropped");
        };
        let length = obu.bytes_used;
        sequence_parsed = obu.header.obu_type == ObuType::SequenceHeader;
        parser.parse(obu).unwrap();
        data = &data[length..];
    }

    let inter_frame_header = [0x1a, 0x01, 0x20];
    let ObuAction::Process(obu) = parser.read_obu(&inter_frame_header).unwrap() else {
        panic!("the frame header was dropped");
    };

    assert!(matches!(
        parser.parse(obu),
        Err(DecodeError::Malformed {
            codec: Codec::Av1,
            unit: BitstreamUnit::Obu,
            ..
        })
    ));
}

#[test]
fn every_fixture_parses_through_cros_codecs() {
    for name in [
        "av1-420-8bit.ivf",
        "av1-420-10bit.ivf",
        "av1-444-8bit.ivf",
        "av1-420-8bit-altref.ivf",
    ] {
        let mut parser = ObuParser::new();

        for frame in common::ivf_frames(Codec::Av1, &common::fixture(name)) {
            feed(&mut parser, &frame.data).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }
}

#[test]
fn sixty_five_tile_columns_never_reach_cros_codecs() {
    let shape = Shape::new(4160, 64);
    let mut parser = ObuParser::new();

    let data = packet(&[
        (SEQUENCE_HEADER, sequence_header(shape)),
        (FRAME_HEADER, key_frame_header(shape, explicit_columns(65))),
    ]);

    assert!(matches!(
        feed(&mut parser, &data),
        Err(DecodeError::Av1Syntax(
            Av1SyntaxError::TileColumnsOutOfRange(65)
        ))
    ));
}

#[test]
fn an_oversized_tile_never_reaches_cros_codecs() {
    let shape = Shape::new(128, 64);
    let mut parser = ObuParser::new();

    let mut tiles = tile_group(whole_frame, &[]);
    tiles.extend([0xff, 1, 2, 3]);
    let data = packet(&[
        (SEQUENCE_HEADER, sequence_header(shape)),
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TILE_GROUP, tiles),
    ]);

    assert!(matches!(
        feed(&mut parser, &data),
        Err(DecodeError::Av1Syntax(Av1SyntaxError::TileSizePastData {
            tile: 0,
            ..
        }))
    ));
}

#[test]
fn a_crafted_two_tile_frame_decodes_through_cros_codecs() {
    let shape = Shape::new(128, 64);
    let mut parser = ObuParser::new();

    let data = packet(&[
        (SEQUENCE_HEADER, sequence_header(shape)),
        (FRAME_HEADER, key_frame_header(shape, two_columns)),
        (TILE_GROUP, tile_group(whole_frame, &[&[1, 2], &[3]])),
    ]);

    feed(&mut parser, &data).unwrap();
}

#[test]
fn an_obu_outside_the_operating_point_is_skipped_whole() {
    let shape = Shape {
        operating_point_idc: 0x101,
        ..Shape::new(64, 64)
    };
    let mut parser = ObuParser::new();

    let mut data = packet(&[(SEQUENCE_HEADER, sequence_header(shape))]);
    data.extend([0x2e, 0x20, 0x02, 0x00, 0x00]);

    feed(&mut parser, &data).unwrap();
}
