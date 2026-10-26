const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_HAS_SIZE_FIELD: u8 = 0b010;

pub(crate) fn temporal_delimiter() -> Vec<u8> {
    vec![(OBU_TEMPORAL_DELIMITER << 3) | OBU_HAS_SIZE_FIELD, 0]
}

pub(crate) fn starts_with_temporal_delimiter(data: &[u8]) -> bool {
    data.first()
        .is_some_and(|&byte| (byte >> 3) & 0x0f == OBU_TEMPORAL_DELIMITER)
}

pub(crate) fn delimited(mut data: Vec<u8>) -> Vec<u8> {
    if starts_with_temporal_delimiter(&data) {
        return data;
    }

    let mut unit = temporal_delimiter();
    unit.append(&mut data);
    unit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporal_delimiter_is_an_empty_sized_obu() {
        assert_eq!(temporal_delimiter(), vec![0x12, 0x00]);
        assert!(starts_with_temporal_delimiter(&temporal_delimiter()));
    }

    #[test]
    fn delimited_prepends_only_when_missing() {
        let frame = vec![0x32, 0x01, 0xff];

        assert_eq!(delimited(frame.clone()), vec![0x12, 0x00, 0x32, 0x01, 0xff]);
        assert_eq!(delimited(delimited(frame.clone())), delimited(frame));
    }
}
