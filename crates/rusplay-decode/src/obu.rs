use cros_codecs::codec::av1::parser::Parser;
use rusplay_core::Codec;

use crate::{BitstreamUnit, DecodeError};

const CODEC: Codec = Codec::Av1;
const TEMPORAL_DELIMITER: [u8; 2] = [0x12, 0x00];
const RESERVED_BIT: u8 = 0x01;
const HAS_SIZE_FIELD: u8 = 0x02;
const EXTENSION_FLAG: u8 = 0x04;
const LEB128_MAX_BYTES: usize = 8;
const LEB128_PAYLOAD_BITS: usize = 7;
const LEB128_PAYLOAD_MASK: u8 = 0x7f;
const LEB128_CONTINUATION: u8 = 0x80;

pub(crate) fn low_overhead_parser() -> Parser {
    let mut parser = Parser::default();
    let _ = parser.read_obu(&TEMPORAL_DELIMITER);
    parser
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
