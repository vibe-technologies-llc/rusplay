use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Walk {
    pub bits: BTreeMap<(char, u8), usize>,
    pub depth: i32,
    pub minimum_depth: i32,
}

pub(super) fn walk(descriptor: &[u8]) -> Walk {
    let mut walk = Walk::default();
    let (mut id, mut size, mut count) = (0u8, 0usize, 0usize);
    let mut position = 0;

    while position < descriptor.len() {
        let prefix = descriptor[position];
        let length = match prefix & 3 {
            3 => 4,
            other => usize::from(other),
        };
        let data = &descriptor[position + 1..position + 1 + length];
        let value = data
            .iter()
            .rev()
            .fold(0usize, |value, byte| (value << 8) | usize::from(*byte));
        position += 1 + length;

        match (prefix >> 2 & 3, prefix >> 4) {
            (1, 7) => size = value,
            (1, 8) => id = u8::try_from(value).unwrap(),
            (1, 9) => count = value,
            (0, 8) => *walk.bits.entry(('i', id)).or_default() += size * count,
            (0, 9) => *walk.bits.entry(('o', id)).or_default() += size * count,
            (0, 11) => *walk.bits.entry(('f', id)).or_default() += size * count,
            (0, 10) => walk.depth += 1,
            (0, 12) => {
                walk.depth -= 1;
                walk.minimum_depth = walk.minimum_depth.min(walk.depth);
            }
            _ => {}
        }
    }

    walk
}
