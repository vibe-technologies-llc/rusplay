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
        Self::build_with_phase(kernel, source_len, target_len, 0.0)
    }

    pub fn build_with_phase(kernel: Kernel, source_len: u32, target_len: u32, phase: f64) -> Self {
        let rows: Vec<Taps> = (0..target_len)
            .map(|index| Taps::separable_with_phase(kernel, source_len, target_len, index, phase))
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

impl TapTable {
    pub fn identity(len: u32) -> Self {
        let positions: Vec<i32> = (0..len).map(|index| saturate(i64::from(index))).collect();

        Self {
            taps_per_pixel: 1,
            firsts: positions.clone(),
            anchors: positions,
            weights: vec![1.0; len as usize],
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

    #[test]
    fn identity_table_passes_every_pixel_through() {
        let table = TapTable::identity(3);

        assert_eq!(table.taps_per_pixel, 1);
        assert_eq!(table.firsts, vec![0, 1, 2]);
        assert_eq!(table.weights, vec![1.0; 3]);
    }
}
