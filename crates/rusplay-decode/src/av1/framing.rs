use rusplay_core::Codec;

use crate::{BitstreamUnit, DecodeError};

const RESERVED_BIT: u8 = 0x01;
const HAS_SIZE_FIELD: u8 = 0x02;
const EXTENSION_FLAG: u8 = 0x04;
const TYPE_SHIFT: u8 = 3;
const TYPE_MASK: u8 = 0x0f;
const TEMPORAL_ID_SHIFT: u8 = 5;
const SPATIAL_ID_SHIFT: u8 = 3;
const SPATIAL_ID_MASK: u8 = 0x03;
const LEB128_MAX_BYTES: usize = 8;
const LEB128_PAYLOAD_BITS: usize = 7;
const LEB128_PAYLOAD_MASK: u8 = 0x7f;
const LEB128_CONTINUATION: u8 = 0x80;

const SEQUENCE_HEADER: u8 = 1;
const TEMPORAL_DELIMITER: u8 = 2;
const FRAME_HEADER: u8 = 3;
const TILE_GROUP: u8 = 4;
const REDUNDANT_FRAME_HEADER: u8 = 7;
const FRAME: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObuKind {
    SequenceHeader,
    TemporalDelimiter,
    FrameHeader,
    TileGroup,
    Frame,
    RedundantFrameHeader,
    Other,
}

impl ObuKind {
    fn from_type(value: u8) -> Self {
        match value {
            SEQUENCE_HEADER => Self::SequenceHeader,
            TEMPORAL_DELIMITER => Self::TemporalDelimiter,
            FRAME_HEADER => Self::FrameHeader,
            TILE_GROUP => Self::TileGroup,
            FRAME => Self::Frame,
            REDUNDANT_FRAME_HEADER => Self::RedundantFrameHeader,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Unit<'a> {
    pub(crate) kind: ObuKind,
    pub(crate) layer: Option<Layer>,
    pub(crate) payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Layer {
    pub(crate) temporal_id: u8,
    pub(crate) spatial_id: u8,
}

#[cfg(any(feature = "vaapi", feature = "nvdec"))]
pub(crate) fn check_framing(data: &[u8]) -> Result<(), DecodeError> {
    units(data).map(drop)
}

#[cfg(any(feature = "vaapi", feature = "nvdec"))]
pub(crate) fn obu_length(data: &[u8]) -> Result<usize, DecodeError> {
    if data.is_empty() {
        return Err(malformed("truncated obu header"));
    }
    Ok(unit_length(data)?.total)
}

pub(super) fn units(data: &[u8]) -> Result<Vec<Unit<'_>>, DecodeError> {
    let mut rest = data;
    let mut units = Vec::new();

    while !rest.is_empty() {
        let length = unit_length(rest)?;
        let header = rest[0];
        let layer = (header & EXTENSION_FLAG != 0).then(|| {
            let extension = rest[1];
            Layer {
                temporal_id: extension >> TEMPORAL_ID_SHIFT,
                spatial_id: (extension >> SPATIAL_ID_SHIFT) & SPATIAL_ID_MASK,
            }
        });

        units.push(Unit {
            kind: ObuKind::from_type((header >> TYPE_SHIFT) & TYPE_MASK),
            layer,
            payload: &rest[length.payload_start..length.total],
        });
        rest = &rest[length.total..];
    }

    Ok(units)
}

struct UnitLength {
    payload_start: usize,
    total: usize,
}

fn unit_length(data: &[u8]) -> Result<UnitLength, DecodeError> {
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
        Ok(size) if size <= remaining => Ok(UnitLength {
            payload_start,
            total: payload_start + size,
        }),
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
        codec: Codec::Av1,
        unit: BitstreamUnit::Obu,
        reason: String::from(reason),
    }
}
