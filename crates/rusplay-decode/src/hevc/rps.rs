use super::{bits::Bits, stop::Stop};

const MAX_DELTA_POC_MINUS1: u32 = 32767;
const MAX_REFERENCED_DELTA_POCS: usize = 63;

#[derive(Debug, Clone, Default)]
pub(super) struct ShortTermSet {
    pub(super) negative: Vec<Picture>,
    pub(super) positive: Vec<Picture>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Picture {
    delta_poc: i32,
    pub(super) used: bool,
}

impl ShortTermSet {
    pub(super) fn used_count(&self) -> usize {
        self.negative
            .iter()
            .chain(&self.positive)
            .filter(|picture| picture.used)
            .count()
    }

    fn delta_pocs(&self) -> usize {
        self.negative.len() + self.positive.len()
    }
}

pub(super) struct Sets<'a> {
    pub(super) existing: &'a [ShortTermSet],
    pub(super) declared: usize,
    pub(super) max_dec_pic_buffering_minus1: u8,
}

pub(super) fn scan_short_term_set(
    bits: &mut Bits,
    sets: &Sets,
    index: usize,
) -> Result<ShortTermSet, Stop> {
    let predicted = index != 0 && bits.bit()?;

    if predicted {
        predict(bits, sets, index)
    } else {
        explicit(bits, sets.max_dec_pic_buffering_minus1)
    }
}

fn explicit(bits: &mut Bits, max_dec_pic_buffering_minus1: u8) -> Result<ShortTermSet, Stop> {
    let maximum = u32::from(max_dec_pic_buffering_minus1);
    let negative = bits.unsigned_max(maximum)?;
    let positive = bits.unsigned_max(maximum - negative)?;

    let mut set = ShortTermSet::default();
    let mut delta_poc = 0_i32;
    for _ in 0..negative {
        let step = bits.unsigned_max(MAX_DELTA_POC_MINUS1)?;
        delta_poc = delta_poc.wrapping_sub(i32::try_from(step).unwrap_or(0) + 1);
        set.negative.push(Picture {
            delta_poc,
            used: bits.bit()?,
        });
    }

    let mut delta_poc = 0_i32;
    for _ in 0..positive {
        let step = bits.unsigned_max(MAX_DELTA_POC_MINUS1)?;
        delta_poc = delta_poc.wrapping_add(i32::try_from(step).unwrap_or(0) + 1);
        set.positive.push(Picture {
            delta_poc,
            used: bits.bit()?,
        });
    }

    Ok(set)
}

fn predict(bits: &mut Bits, sets: &Sets, index: usize) -> Result<ShortTermSet, Stop> {
    let delta_idx_minus1 = if index == sets.declared {
        bits.unsigned_max(u32::try_from(index - 1).unwrap_or(0))?
    } else {
        0
    };
    let negative_sign = bits.bit()?;
    let magnitude = i32::try_from(bits.unsigned_max(MAX_DELTA_POC_MINUS1)?).unwrap_or(0) + 1;
    let delta_rps = if negative_sign { -magnitude } else { magnitude };

    let reference_index = index - (delta_idx_minus1 as usize + 1);
    let reference = sets
        .existing
        .get(reference_index)
        .ok_or(Stop::ParserRejects)?;
    if reference.delta_pocs() > MAX_REFERENCED_DELTA_POCS {
        return Err(Stop::ShortTermSetSize);
    }

    let last = reference.delta_pocs();
    let mut used = Vec::with_capacity(last + 1);
    let mut kept = Vec::with_capacity(last + 1);
    for _ in 0..=last {
        let flag = bits.bit()?;
        used.push(flag);
        kept.push(flag || bits.bit()?);
    }

    let negative_count = reference.negative.len();
    let shifted = |picture: &Picture| picture.delta_poc.wrapping_add(delta_rps);
    let mut set = ShortTermSet::default();

    for (offset, picture) in reference.positive.iter().enumerate().rev() {
        let slot = negative_count + offset;
        let delta_poc = shifted(picture);
        if delta_poc < 0 && kept[slot] {
            set.negative.push(Picture {
                delta_poc,
                used: used[slot],
            });
        }
    }
    if delta_rps < 0 && kept[last] {
        set.negative.push(Picture {
            delta_poc: delta_rps,
            used: used[last],
        });
    }
    for (slot, picture) in reference.negative.iter().enumerate() {
        let delta_poc = shifted(picture);
        if delta_poc < 0 && kept[slot] {
            set.negative.push(Picture {
                delta_poc,
                used: used[slot],
            });
        }
    }

    for (slot, picture) in reference.negative.iter().enumerate().rev() {
        let delta_poc = shifted(picture);
        if delta_poc > 0 && kept[slot] {
            set.positive.push(Picture {
                delta_poc,
                used: used[slot],
            });
        }
    }
    if delta_rps > 0 && kept[last] {
        set.positive.push(Picture {
            delta_poc: delta_rps,
            used: used[last],
        });
    }
    for (offset, picture) in reference.positive.iter().enumerate() {
        let slot = negative_count + offset;
        let delta_poc = shifted(picture);
        if delta_poc > 0 && kept[slot] {
            set.positive.push(Picture {
                delta_poc,
                used: used[slot],
            });
        }
    }

    Ok(set)
}
