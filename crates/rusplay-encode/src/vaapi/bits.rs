use crate::vaapi::va::PackedHeader;

const START_CODE: [u8; 4] = [0, 0, 0, 1];
const EMULATION_PREVENTION_BYTE: u8 = 3;

#[derive(Debug, Clone, Default)]
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    length: usize,
}

impl BitWriter {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn position(&self) -> usize {
        self.length
    }

    pub(super) fn flag(&mut self, value: bool) {
        if self.length.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if value {
            let shift = 7 - self.length % 8;
            if let Some(last) = self.bytes.last_mut() {
                *last |= 1 << shift;
            }
        }
        self.length += 1;
    }

    pub(super) fn bits(&mut self, value: u64, count: u32) {
        for bit in (0..count).rev() {
            self.flag((value >> bit) & 1 == 1);
        }
    }

    pub(super) fn ue(&mut self, value: u32) {
        let coded = u64::from(value) + 1;
        let length = coded.bit_width();

        self.bits(0, length - 1);
        self.bits(coded, length);
    }

    pub(super) fn se(&mut self, value: i32) {
        let magnitude = value.unsigned_abs();
        let mapped = if value > 0 {
            2 * magnitude - 1
        } else {
            2 * magnitude
        };

        self.ue(mapped);
    }

    pub(super) fn trailing_bits(&mut self) {
        self.flag(true);
        while !self.length.is_multiple_of(8) {
            self.flag(false);
        }
    }

    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

pub(super) fn nal_unit(header: &[u8], rbsp: BitWriter) -> Vec<u8> {
    let payload = rbsp.into_bytes();
    let mut nal = Vec::with_capacity(START_CODE.len() + header.len() + payload.len() + 8);
    nal.extend_from_slice(&START_CODE);
    nal.extend_from_slice(header);

    let mut zeros = 0;
    for byte in payload {
        if zeros >= 2 && byte <= EMULATION_PREVENTION_BYTE {
            nal.push(EMULATION_PREVENTION_BYTE);
            zeros = 0;
        }
        nal.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }

    nal
}

pub(super) fn nal_unit_prefix(header: &[u8], rbsp: BitWriter) -> PackedHeader {
    let padding = (8 - rbsp.position() % 8) % 8;
    let bytes = nal_unit(header, rbsp);

    PackedHeader {
        bits: bytes.len() * 8 - padding,
        bytes,
    }
}

pub(super) fn packed(bytes: Vec<u8>) -> PackedHeader {
    PackedHeader {
        bits: bytes.len() * 8,
        bytes,
    }
}

pub(super) fn leb128(value: usize, width: usize) -> Vec<u8> {
    let mut remaining = value;
    let mut bytes = Vec::with_capacity(width.max(1));
    loop {
        let low = u8::try_from(remaining & 0x7f).unwrap_or(0);
        remaining >>= 7;
        let more = remaining != 0 || bytes.len() + 1 < width;
        bytes.push(if more { low | 0x80 } else { low });
        if !more {
            return bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb_codes_match_the_spec_table() {
        let mut writer = BitWriter::new();

        writer.ue(0);
        writer.ue(1);
        writer.ue(2);
        writer.ue(3);
        writer.se(-1);
        writer.trailing_bits();

        assert_eq!(writer.into_bytes(), vec![0b1010_0110, 0b0100_0111]);
    }

    #[test]
    fn emulation_prevention_breaks_start_code_patterns() {
        let mut rbsp = BitWriter::new();
        rbsp.bits(0x0000_0100_0003, 48);

        let nal = nal_unit(&[0x67], rbsp);

        assert_eq!(nal, vec![0, 0, 0, 1, 0x67, 0, 0, 3, 1, 0, 0, 3, 3]);
    }

    #[test]
    fn leb128_pads_to_the_requested_width() {
        assert_eq!(leb128(5, 1), vec![5]);
        assert_eq!(leb128(5, 2), vec![0x85, 0]);
        assert_eq!(leb128(300, 1), vec![0xac, 0x02]);
    }
}
