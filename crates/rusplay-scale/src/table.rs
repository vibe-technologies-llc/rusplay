use crate::{Kernel, Taps};

#[derive(Debug, Clone, PartialEq)]
pub struct TapTable {
    pub taps_per_pixel: u32,
    pub firsts: Vec<i32>,
    pub anchors: Vec<i32>,
    pub weights: Vec<f32>,
}

impl TapTable {
    pub fn build(kernel: Kernel, source_len: u32, target_len: u32) -> Self {
        let rows: Vec<Taps> = (0..target_len)
            .map(|index| Taps::separable(kernel, source_len, target_len, index))
            .collect();
        let width = rows
            .iter()
            .map(|taps| taps.weights.len())
            .max()
            .unwrap_or(0);

        let firsts = rows.iter().map(|taps| saturate(taps.first)).collect();
        let anchors = rows.iter().map(|taps| saturate(taps.anchor)).collect();
        let weights = rows
            .iter()
            .flat_map(|taps| {
                let padding = width - taps.weights.len();
                taps.weights
                    .iter()
                    .copied()
                    .chain(std::iter::repeat_n(0.0, padding))
            })
            .collect();

        Self {
            taps_per_pixel: u32::try_from(width).unwrap_or(u32::MAX),
            firsts,
            anchors,
            weights,
        }
    }
}

fn saturate(position: i64) -> i32 {
    i32::try_from(position).unwrap_or(if position < 0 { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
mod tests {
    use rusplay_core::ScaleFilter;

    use super::*;

    #[test]
    fn table_is_rectangular() {
        let table = TapTable::build(Kernel::new(ScaleFilter::Hermite), 2560, 1920);

        assert_eq!(table.firsts.len(), 1920);
        assert_eq!(table.anchors.len(), 1920);
        assert_eq!(table.weights.len(), 1920 * table.taps_per_pixel as usize);
    }
}
