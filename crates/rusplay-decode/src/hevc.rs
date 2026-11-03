use rusplay_core::Codec;

use crate::{BitstreamUnit, DecodeError, signal::unescape};

const CODEC: Codec = Codec::Hevc;
const NAL_HEADER_LEN: usize = 2;
const MAX_SUB_LAYERS_MINUS1: u64 = 6;
const SUB_LAYER_SLOTS: u64 = 8;
const PROFILE_BITS: usize = 88;
const LEVEL_BITS: usize = 8;
const VPS_LAYER_BITS: usize = 6;
const SIZE_IDS: usize = 4;
const MATRIX_IDS: u64 = 6;
const LARGEST_SIZE_ID: usize = 3;
const LARGEST_SIZE_ID_MATRIX_STEP: u64 = 3;
const LONGEST_SCALING_LIST: usize = 64;
const SEPARATE_COLOUR_PLANE_CHROMA_FORMAT: u64 = 3;
const LARGEST_SIZE_ID_WITHOUT_DC: usize = 1;

#[derive(Debug, Clone, Copy)]
enum Stop {
    Truncated,
    SubLayerCount,
    ScalingListReference,
}

impl Stop {
    fn reason(self) -> Option<&'static str> {
        match self {
            Self::Truncated => None,
            Self::SubLayerCount => Some("sub layer count above the maximum of seven"),
            Self::ScalingListReference => {
                Some("scaling list predicted from a matrix that does not precede it")
            }
        }
    }
}

struct Bits {
    data: Vec<u8>,
    position: usize,
}

impl Bits {
    fn after_header(nalu: &[u8]) -> Self {
        Self {
            data: unescape(nalu.get(NAL_HEADER_LEN..).unwrap_or_default()),
            position: 0,
        }
    }

    fn bit(&mut self) -> Result<bool, Stop> {
        let byte = self.data.get(self.position / 8).ok_or(Stop::Truncated)?;
        let bit = (byte >> (7 - self.position % 8)) & 1;
        self.position += 1;

        Ok(bit == 1)
    }

    fn bits(&mut self, count: usize) -> Result<u64, Stop> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | u64::from(self.bit()?);
        }

        Ok(value)
    }

    fn skip(&mut self, count: usize) -> Result<(), Stop> {
        let end = self.position.checked_add(count).ok_or(Stop::Truncated)?;
        if end > self.data.len() * 8 {
            return Err(Stop::Truncated);
        }
        self.position = end;

        Ok(())
    }

    fn unsigned(&mut self) -> Result<u64, Stop> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > u32::BITS as usize {
                return Err(Stop::Truncated);
            }
        }

        Ok((1 << zeros) - 1 + self.bits(zeros)?)
    }

    fn signed(&mut self) -> Result<(), Stop> {
        self.unsigned().map(drop)
    }
}

fn conclude(scan: Result<(), Stop>, unit: BitstreamUnit) -> Result<(), DecodeError> {
    match scan.err().and_then(Stop::reason) {
        Some(reason) => Err(DecodeError::Malformed {
            codec: CODEC,
            unit,
            reason: String::from(reason),
        }),
        None => Ok(()),
    }
}

fn sub_layer_count(bits: &mut Bits) -> Result<usize, Stop> {
    let minus1 = bits.bits(3)?;
    if minus1 > MAX_SUB_LAYERS_MINUS1 {
        return Err(Stop::SubLayerCount);
    }

    Ok(usize::try_from(minus1).unwrap_or_default())
}

pub(crate) fn check_video_parameter_set(nalu: &[u8]) -> Result<(), DecodeError> {
    let scan = (|| {
        let mut bits = Bits::after_header(nalu);
        bits.skip(4 + 1 + 1 + VPS_LAYER_BITS)?;
        sub_layer_count(&mut bits).map(drop)
    })();

    conclude(scan, BitstreamUnit::VideoParameterSet)
}

pub(crate) fn check_sequence_parameter_set(nalu: &[u8]) -> Result<(), DecodeError> {
    conclude(
        scan_sequence_parameter_set(&mut Bits::after_header(nalu)),
        BitstreamUnit::SequenceParameterSet,
    )
}

pub(crate) fn check_picture_parameter_set(nalu: &[u8]) -> Result<(), DecodeError> {
    conclude(
        scan_picture_parameter_set(&mut Bits::after_header(nalu)),
        BitstreamUnit::PictureParameterSet,
    )
}

fn scan_profile_tier_level(bits: &mut Bits, sub_layers: usize) -> Result<(), Stop> {
    bits.skip(PROFILE_BITS + LEVEL_BITS)?;

    let mut present = Vec::with_capacity(sub_layers);
    for _ in 0..sub_layers {
        present.push((bits.bit()?, bits.bit()?));
    }
    if sub_layers > 0 {
        let reserved = SUB_LAYER_SLOTS - u64::try_from(sub_layers).unwrap_or_default();
        bits.skip(usize::try_from(reserved * 2).unwrap_or_default())?;
    }
    for (profile, level) in present {
        if profile {
            bits.skip(PROFILE_BITS)?;
        }
        if level {
            bits.skip(LEVEL_BITS)?;
        }
    }

    Ok(())
}

fn scan_sequence_parameter_set(bits: &mut Bits) -> Result<(), Stop> {
    bits.skip(4)?;
    let sub_layers = sub_layer_count(bits)?;
    bits.skip(1)?;
    scan_profile_tier_level(bits, sub_layers)?;

    bits.unsigned()?;
    if bits.unsigned()? == SEPARATE_COLOUR_PLANE_CHROMA_FORMAT {
        bits.skip(1)?;
    }
    bits.unsigned()?;
    bits.unsigned()?;
    if bits.bit()? {
        for _ in 0..4 {
            bits.unsigned()?;
        }
    }
    for _ in 0..3 {
        bits.unsigned()?;
    }

    let ordering_info_for_every_layer = bits.bit()?;
    let first_ordered = if ordering_info_for_every_layer {
        0
    } else {
        sub_layers
    };
    for _ in first_ordered..=sub_layers {
        for _ in 0..3 {
            bits.unsigned()?;
        }
    }

    for _ in 0..6 {
        bits.unsigned()?;
    }

    let scaling_list_enabled = bits.bit()?;
    if scaling_list_enabled && bits.bit()? {
        scan_scaling_list_data(bits)?;
    }

    Ok(())
}

fn scan_picture_parameter_set(bits: &mut Bits) -> Result<(), Stop> {
    bits.unsigned()?;
    bits.unsigned()?;
    bits.skip(2 + 3 + 1 + 1)?;
    bits.unsigned()?;
    bits.unsigned()?;
    bits.signed()?;
    bits.skip(2)?;

    if bits.bit()? {
        bits.unsigned()?;
    }
    bits.signed()?;
    bits.signed()?;
    bits.skip(3 + 1)?;

    let tiles_enabled = bits.bit()?;
    bits.skip(1)?;
    if tiles_enabled {
        let columns = bits.unsigned()?;
        let rows = bits.unsigned()?;
        if !bits.bit()? {
            for _ in 0..columns.saturating_add(rows) {
                bits.unsigned()?;
            }
        }
        bits.skip(1)?;
    }

    bits.skip(1)?;
    if bits.bit()? {
        bits.skip(1)?;
        if !bits.bit()? {
            bits.signed()?;
            bits.signed()?;
        }
    }

    if bits.bit()? {
        scan_scaling_list_data(bits)?;
    }

    Ok(())
}

fn scan_scaling_list_data(bits: &mut Bits) -> Result<(), Stop> {
    for size_id in 0..SIZE_IDS {
        let step = if size_id == LARGEST_SIZE_ID {
            LARGEST_SIZE_ID_MATRIX_STEP
        } else {
            1
        };

        for matrix_id in (0..MATRIX_IDS).step_by(usize::try_from(step).unwrap_or(1)) {
            if bits.bit()? {
                if size_id > LARGEST_SIZE_ID_WITHOUT_DC {
                    bits.signed()?;
                }
                let coefficients = LONGEST_SCALING_LIST.min(1 << (4 + (size_id << 1)));
                for _ in 0..coefficients {
                    bits.signed()?;
                }
            } else if bits.unsigned()?.saturating_mul(step) > matrix_id {
                return Err(Stop::ScalingListReference);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
