use crate::{Av1SyntaxError, BitstreamUnit};

const BYTE_BITS: usize = 8;
const UVLC_SATURATING_ZEROS: u32 = 32;

pub(super) struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    unit: BitstreamUnit,
}

impl<'a> BitReader<'a> {
    pub(super) fn new(data: &'a [u8], unit: BitstreamUnit) -> Self {
        Self {
            data,
            position: 0,
            unit,
        }
    }

    pub(super) fn set_unit(&mut self, unit: BitstreamUnit) {
        self.unit = unit;
    }

    pub(super) fn truncated(&self) -> Av1SyntaxError {
        Av1SyntaxError::Truncated { unit: self.unit }
    }

    pub(super) fn bit(&mut self) -> Result<bool, Av1SyntaxError> {
        let byte = self
            .data
            .get(self.position / BYTE_BITS)
            .ok_or_else(|| self.truncated())?;
        let shift = BYTE_BITS - 1 - self.position % BYTE_BITS;
        self.position += 1;

        Ok((byte >> shift) & 1 == 1)
    }

    pub(super) fn bits(&mut self, count: u32) -> Result<u32, Av1SyntaxError> {
        let mut value = 0u64;
        for _ in 0..count {
            value = (value << 1) | u64::from(self.bit()?);
        }

        u32::try_from(value).map_err(|_| self.truncated())
    }

    pub(super) fn signed(&mut self, count: u32) -> Result<i32, Av1SyntaxError> {
        let value = i64::from(self.bits(count)?);
        let sign = 1i64 << (count - 1);
        let signed = if value & sign == 0 {
            value
        } else {
            value - 2 * sign
        };

        i32::try_from(signed).map_err(|_| self.truncated())
    }

    pub(super) fn uvlc(&mut self) -> Result<u32, Av1SyntaxError> {
        let mut leading_zeros = 0;
        while !self.bit()? {
            leading_zeros += 1;
        }

        if leading_zeros >= UVLC_SATURATING_ZEROS {
            return Ok(u32::MAX);
        }

        let value = self.bits(leading_zeros)?;
        Ok(value.wrapping_add((1 << leading_zeros) - 1))
    }

    pub(super) fn non_symmetric(&mut self, symbols: u32) -> Result<u32, Av1SyntaxError> {
        let width = symbols.ilog2() + 1;
        let threshold = (1 << width) - symbols;
        let value = self.bits(width - 1)?;
        if value < threshold {
            return Ok(value);
        }

        let extra = u32::from(self.bit()?);
        Ok((value << 1) - threshold + extra)
    }

    pub(super) fn little_endian(&mut self, bytes: u32) -> Result<u64, Av1SyntaxError> {
        let mut value = 0u64;
        for index in 0..bytes {
            value |= u64::from(self.bits(8)?) << (8 * index);
        }

        Ok(value)
    }

    pub(super) fn byte_align(&mut self) {
        self.position = self.position.next_multiple_of(BYTE_BITS);
    }

    pub(super) fn remaining_bytes(&self) -> u64 {
        let consumed = self.position.div_ceil(BYTE_BITS);
        u64::try_from(self.data.len().saturating_sub(consumed)).unwrap_or(u64::MAX)
    }

    pub(super) fn skip_bytes(&mut self, bytes: u64) -> Result<(), Av1SyntaxError> {
        if bytes > self.remaining_bytes() {
            return Err(self.truncated());
        }

        let bits = usize::try_from(bytes).map_err(|_| self.truncated())? * BYTE_BITS;
        self.position += bits;
        Ok(())
    }
}
