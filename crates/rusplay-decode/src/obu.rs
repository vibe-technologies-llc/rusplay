use cros_codecs::codec::av1::parser::{
    FrameHeaderObu, FrameType, Obu, ObuAction, ObuType, ParsedObu, Parser,
};
use rusplay_core::Codec;

use crate::{BitstreamUnit, DecodeError};

const CODEC: Codec = Codec::Av1;
const KEY_FRAME: u8 = 0;
const TEMPORAL_DELIMITER: [u8; 2] = [0x12, 0x00];
const RESERVED_BIT: u8 = 0x01;
const HAS_SIZE_FIELD: u8 = 0x02;
const EXTENSION_FLAG: u8 = 0x04;
const LEB128_MAX_BYTES: usize = 8;
const LEB128_PAYLOAD_BITS: usize = 7;
const LEB128_PAYLOAD_MASK: u8 = 0x7f;
const LEB128_CONTINUATION: u8 = 0x80;

const ALL_REFERENCE_SLOTS: u32 = 0xff;
const SHOW_EXISTING_FRAME: u8 = 0x80;
const FRAME_TYPE_SHIFT: u8 = 5;
const FRAME_TYPE_MASK: u8 = 0x03;
const SHOW_FRAME: u8 = 0x10;

pub(crate) struct ObuParser {
    parser: Parser,
    references_valid: bool,
}

impl ObuParser {
    pub(crate) fn new() -> Self {
        let mut parser = Parser::default();
        let _ = parser.read_obu(&TEMPORAL_DELIMITER);

        Self {
            parser,
            references_valid: false,
        }
    }

    pub(crate) fn read_obu<'a>(&mut self, data: &'a [u8]) -> Result<ObuAction<'a>, DecodeError> {
        self.parser.read_obu(data).map_err(obu_error)
    }

    pub(crate) fn parse<'a>(&mut self, obu: Obu<'a>) -> Result<ParsedObu<'a>, DecodeError> {
        let kind = obu.header.obu_type;

        if kind == ObuType::TileGroup && self.parser.last_frame_header.is_none() {
            return Err(malformed("tile group without a frame header"));
        }
        if matches!(kind, ObuType::FrameHeader | ObuType::Frame) {
            self.admit_frame_header(obu.as_ref())?;
        }

        let parsed = self.parser.parse_obu(obu).map_err(obu_error);
        if parsed.is_err() {
            self.references_valid = false;
        }

        parsed
    }

    pub(crate) fn ref_frame_update(&mut self, header: &FrameHeaderObu) -> Result<(), DecodeError> {
        self.parser
            .ref_frame_update(header)
            .map_err(|reason| DecodeError::Malformed {
                codec: CODEC,
                unit: BitstreamUnit::FrameHeader,
                reason,
            })?;

        let refreshes_every_slot = header.frame_type == FrameType::KeyFrame
            && header.refresh_frame_flags == ALL_REFERENCE_SLOTS;
        self.references_valid |= refreshes_every_slot;

        Ok(())
    }

    fn admit_frame_header(&mut self, payload: &[u8]) -> Result<(), DecodeError> {
        let Some(sequence) = &self.parser.sequence_header else {
            return Err(malformed("frame header without a sequence header"));
        };
        if sequence.reduced_still_picture_header {
            return Ok(());
        }

        let first = payload.first().copied().unwrap_or_default();
        let key_frame = first & SHOW_EXISTING_FRAME == 0
            && (first >> FRAME_TYPE_SHIFT) & FRAME_TYPE_MASK == KEY_FRAME;

        if !key_frame && !self.references_valid {
            return Err(malformed(
                "inter frame before a key frame refreshed the references",
            ));
        }
        if key_frame && first & SHOW_FRAME != 0 {
            self.references_valid = false;
        }

        Ok(())
    }
}

fn obu_error(reason: String) -> DecodeError {
    DecodeError::Malformed {
        codec: CODEC,
        unit: BitstreamUnit::Obu,
        reason,
    }
}

pub(crate) fn check_framing(data: &[u8]) -> Result<(), DecodeError> {
    let mut rest = data;

    while !rest.is_empty() {
        rest = rest
            .get(unit_length(rest)?..)
            .ok_or_else(|| malformed("obu runs past the end of the packet"))?;
    }

    Ok(())
}

fn unit_length(data: &[u8]) -> Result<usize, DecodeError> {
    let header = data[0];
    if header & RESERVED_BIT != 0 {
        return Err(malformed("reserved bit set in the obu header"));
    }
    if header & HAS_SIZE_FIELD == 0 {
        return Err(malformed("obu without a size field"));
    }

    let header_length = if header & EXTENSION_FLAG != 0 { 2 } else { 1 };
    let (size, size_length) = leb128(data.get(header_length..).unwrap_or_default())?;
    let payload_start = header_length + size_length;
    let remaining = data.len() - payload_start;

    match usize::try_from(size) {
        Ok(size) if size <= remaining => Ok(payload_start + size),
        _ => Err(malformed("obu size runs past the end of the packet")),
    }
}

fn leb128(data: &[u8]) -> Result<(u64, usize), DecodeError> {
    let mut value = 0u64;

    for (index, shift) in (0..LEB128_MAX_BYTES).zip((0..).step_by(LEB128_PAYLOAD_BITS)) {
        let byte = *data
            .get(index)
            .ok_or_else(|| malformed("truncated obu size"))?;
        value |= u64::from(byte & LEB128_PAYLOAD_MASK) << shift;
        if byte & LEB128_CONTINUATION == 0 {
            return Ok((value, index + 1));
        }
    }

    Err(malformed("unterminated obu size"))
}

fn malformed(reason: &str) -> DecodeError {
    DecodeError::Malformed {
        codec: CODEC,
        unit: BitstreamUnit::Obu,
        reason: String::from(reason),
    }
}

#[cfg(test)]
mod tests;
