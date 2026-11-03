use super::stop::Stop;

const MAX_READ_BITS: usize = 31;
const MAX_ZERO_PREFIX: usize = 31;
const EMULATION_PREVENTION_BYTE: u8 = 3;
const NO_ZEROS: u16 = 0xffff;

pub(super) struct Bits<'a> {
    data: &'a [u8],
    next: usize,
    current: u8,
    left: usize,
    window: u16,
}

pub(super) fn ceil_log2(value: u32) -> usize {
    match value {
        0 | 1 => 0,
        _ => count((value - 1).bit_width()),
    }
}

pub(super) fn count(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

impl<'a> Bits<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            next: 0,
            current: 0,
            left: 0,
            window: NO_ZEROS,
        }
    }

    fn raw_byte(&mut self) -> Result<u8, Stop> {
        let byte = *self.data.get(self.next).ok_or(Stop::ParserRejects)?;
        self.next += 1;

        Ok(byte)
    }

    fn load_byte(&mut self) -> Result<(), Stop> {
        let mut byte = self.raw_byte()?;
        if self.window == 0 && byte == EMULATION_PREVENTION_BYTE {
            byte = self.raw_byte()?;
            self.window = NO_ZEROS;
        }
        self.window = (self.window << 8) | u16::from(byte);
        self.current = byte;
        self.left = 8;

        Ok(())
    }

    pub(super) fn bit(&mut self) -> Result<bool, Stop> {
        if self.left == 0 {
            self.load_byte()?;
        }
        self.left -= 1;

        Ok((self.current >> self.left) & 1 == 1)
    }

    pub(super) fn bits(&mut self, count: usize) -> Result<u32, Stop> {
        if count > MAX_READ_BITS {
            return Err(Stop::ParserRejects);
        }

        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.bit()?);
        }

        Ok(value)
    }

    pub(super) fn skip(&mut self, count: usize) -> Result<(), Stop> {
        for _ in 0..count {
            self.bit()?;
        }

        Ok(())
    }

    pub(super) fn unsigned(&mut self) -> Result<u32, Stop> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > MAX_ZERO_PREFIX {
                return Err(Stop::ParserRejects);
            }
        }

        Ok((1 << zeros) - 1 + self.bits(zeros)?)
    }

    pub(super) fn unsigned_max(&mut self, max: u32) -> Result<u32, Stop> {
        let value = self.unsigned()?;
        if value > max {
            return Err(Stop::ParserRejects);
        }

        Ok(value)
    }

    pub(super) fn signed(&mut self) -> Result<(), Stop> {
        self.unsigned().map(drop)
    }
}
