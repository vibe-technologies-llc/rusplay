const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_HAS_SIZE_FIELD: u8 = 0b010;

const TEMPORAL_DELIMITER_LEN: usize = 2;
const TEMPORAL_DELIMITER: [u8; TEMPORAL_DELIMITER_LEN] =
    [(OBU_TEMPORAL_DELIMITER << 3) | OBU_HAS_SIZE_FIELD, 0];

#[cfg(test)]
pub(crate) fn temporal_delimiter() -> Vec<u8> {
    TEMPORAL_DELIMITER.to_vec()
}

pub(crate) fn starts_with_temporal_delimiter(data: &[u8]) -> bool {
    data.first()
        .is_some_and(|&byte| (byte >> 3) & 0x0f == OBU_TEMPORAL_DELIMITER)
}

#[derive(Debug)]
pub(crate) struct AccessUnit {
    data: Vec<u8>,
    delimit: bool,
}

impl AccessUnit {
    pub(crate) fn new(delimit: bool, mut buffer: Vec<u8>) -> Self {
        buffer.clear();

        Self {
            data: buffer,
            delimit,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        if self.data.is_empty() {
            let delimiter = self.delimit && !starts_with_temporal_delimiter(chunk);
            let room = chunk.len() + if delimiter { TEMPORAL_DELIMITER_LEN } else { 0 };
            self.data.reserve_exact(room);
            if delimiter {
                self.data.extend_from_slice(&TEMPORAL_DELIMITER);
            }
        }

        self.data.extend_from_slice(chunk);
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.data.is_empty() && self.delimit {
            self.data.extend_from_slice(&TEMPORAL_DELIMITER);
        }

        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporal_delimiter_is_an_empty_sized_obu() {
        assert_eq!(temporal_delimiter(), vec![0x12, 0x00]);
        assert!(starts_with_temporal_delimiter(&temporal_delimiter()));
    }

    fn access_unit(delimit: bool, chunks: &[&[u8]]) -> Vec<u8> {
        let mut unit = AccessUnit::new(delimit, Vec::new());
        for chunk in chunks {
            unit.push(chunk);
        }

        unit.finish()
    }

    #[test]
    fn delimiting_prepends_only_when_missing() {
        let frame: &[u8] = &[0x32, 0x01, 0xff];
        let delimited = [0x12, 0x00, 0x32, 0x01, 0xff];

        assert_eq!(access_unit(true, &[frame]), delimited);
        assert_eq!(access_unit(true, &[&delimited]), delimited);
        assert_eq!(access_unit(false, &[frame]), frame);
    }

    #[test]
    fn segments_are_joined_and_delimited_once() {
        let unit = access_unit(true, &[&[0x32, 0x01], &[0xff], &[0x0a]]);

        assert_eq!(unit, [0x12, 0x00, 0x32, 0x01, 0xff, 0x0a]);
    }

    #[test]
    fn an_empty_delimited_unit_is_only_the_delimiter() {
        assert_eq!(access_unit(true, &[]), [0x12, 0x00]);
        assert_eq!(access_unit(false, &[]), Vec::<u8>::new());
    }
}
