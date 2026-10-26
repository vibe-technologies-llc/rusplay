use std::sync::OnceLock;

pub(crate) const BLUE_NOISE_SIZE: usize = 64;
pub(crate) const BAYER_BITS: u32 = 4;

const CELLS: usize = BLUE_NOISE_SIZE * BLUE_NOISE_SIZE;
const SIGMA: f64 = 1.5;
const INITIAL_FILL_DIVISOR: usize = 10;
const SEED: u64 = 0x9e37_79b9_7f4a_7c15;

struct Field {
    ones: Vec<bool>,
    energy: Vec<f64>,
    kernel: Vec<f64>,
}

impl Field {
    fn new() -> Self {
        let kernel = (0..CELLS)
            .map(|offset| {
                let dx = wrapped_distance(offset % BLUE_NOISE_SIZE);
                let dy = wrapped_distance(offset / BLUE_NOISE_SIZE);
                (-(dx * dx + dy * dy) / (2.0 * SIGMA * SIGMA)).exp()
            })
            .collect();

        Self {
            ones: vec![false; CELLS],
            energy: vec![0.0; CELLS],
            kernel,
        }
    }

    fn toggle(&mut self, cell: usize) {
        let sign = if self.ones[cell] { -1.0 } else { 1.0 };
        self.ones[cell] = !self.ones[cell];
        let (cx, cy) = (cell % BLUE_NOISE_SIZE, cell / BLUE_NOISE_SIZE);

        for (index, energy) in self.energy.iter_mut().enumerate() {
            let dx = (index % BLUE_NOISE_SIZE + BLUE_NOISE_SIZE - cx) % BLUE_NOISE_SIZE;
            let dy = (index / BLUE_NOISE_SIZE + BLUE_NOISE_SIZE - cy) % BLUE_NOISE_SIZE;
            *energy += sign * self.kernel[dy * BLUE_NOISE_SIZE + dx];
        }
    }

    fn tightest_cluster(&self) -> usize {
        self.extreme(true, |candidate, best| candidate > best)
    }

    fn largest_void(&self) -> usize {
        self.extreme(false, |candidate, best| candidate < best)
    }

    fn extreme(&self, among_ones: bool, better: impl Fn(f64, f64) -> bool) -> usize {
        let mut chosen = None;

        for (cell, (&one, &energy)) in self.ones.iter().zip(&self.energy).enumerate() {
            if one != among_ones {
                continue;
            }
            if chosen.is_none_or(|(_, best)| better(energy, best)) {
                chosen = Some((cell, energy));
            }
        }

        chosen.map_or(0, |(cell, _)| cell)
    }
}

fn wrapped_distance(offset: usize) -> f64 {
    let wrapped = offset.min(BLUE_NOISE_SIZE - offset);

    f64::from(u32::try_from(wrapped).unwrap_or(u32::MAX))
}

fn shuffled_cells() -> Vec<usize> {
    let mut state = SEED;
    let mut cells: Vec<usize> = (0..CELLS).collect();

    for index in (1..CELLS).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bound = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let pick = usize::try_from(state % bound).unwrap_or(0);
        cells.swap(index, pick);
    }

    cells
}

fn initial_pattern() -> Field {
    let mut field = Field::new();
    for cell in shuffled_cells()
        .into_iter()
        .take(CELLS / INITIAL_FILL_DIVISOR)
    {
        field.toggle(cell);
    }

    loop {
        let cluster = field.tightest_cluster();
        field.toggle(cluster);
        let void = field.largest_void();
        field.toggle(void);
        if void == cluster {
            return field;
        }
    }
}

pub(crate) fn blue_noise_ranks() -> Vec<usize> {
    let initial = initial_pattern();
    let placed = initial.ones.iter().filter(|one| **one).count();
    let mut ranks = vec![0; CELLS];

    let mut removing = Field {
        ones: initial.ones.clone(),
        energy: initial.energy.clone(),
        kernel: initial.kernel.clone(),
    };
    for rank in (0..placed).rev() {
        let cluster = removing.tightest_cluster();
        removing.toggle(cluster);
        ranks[cluster] = rank;
    }

    let mut adding = initial;
    for rank in placed..CELLS {
        let void = adding.largest_void();
        adding.toggle(void);
        ranks[void] = rank;
    }

    ranks
}

pub(crate) fn blue_noise_thresholds() -> &'static [f32] {
    static THRESHOLDS: OnceLock<Vec<f32>> = OnceLock::new();

    THRESHOLDS.get_or_init(generate_thresholds)
}

fn generate_thresholds() -> Vec<f32> {
    let cells = f64::from(u32::try_from(CELLS).unwrap_or(u32::MAX));

    blue_noise_ranks()
        .into_iter()
        .map(|rank| {
            let rank = f64::from(u32::try_from(rank).unwrap_or(u32::MAX));
            #[allow(clippy::cast_possible_truncation)]
            let threshold = ((rank + 0.5) / cells) as f32;
            threshold
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn bayer_rank(x: u32, y: u32) -> u32 {
    (0..BAYER_BITS).fold(0, |rank, bit| {
        let x_bit = (x >> bit) & 1;
        let y_bit = (y >> bit) & 1;
        (rank << 2) | (((x_bit ^ y_bit) << 1) | y_bit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recursive_bayer(size: u32) -> Vec<u32> {
        if size == 1 {
            return vec![0];
        }
        let half = size / 2;
        let inner = recursive_bayer(half);
        let corner = [[0, 2], [3, 1]];

        (0..size * size)
            .map(|index| {
                let (x, y) = (index % size, index / size);
                4 * inner[((y % half) * half + x % half) as usize]
                    + corner[(y / half) as usize][(x / half) as usize]
            })
            .collect()
    }

    #[test]
    fn the_ordered_pattern_is_the_recursive_bayer_matrix() {
        let size = 1 << BAYER_BITS;

        let expected = recursive_bayer(size);

        for y in 0..size {
            for x in 0..size {
                assert_eq!(bayer_rank(x, y), expected[(y * size + x) as usize]);
            }
        }
    }

    #[test]
    fn blue_noise_ranks_every_cell_once_with_little_low_frequency_energy() {
        let ranks = blue_noise_ranks();
        let thresholds = blue_noise_thresholds();
        let block = 8;
        let blocks = BLUE_NOISE_SIZE / block;

        let mut sorted = ranks.clone();
        sorted.sort_unstable();
        let worst_block = (0..blocks * blocks)
            .map(|index| {
                let (bx, by) = (index % blocks, index / blocks);
                let sum: f64 = (0..block * block)
                    .map(|cell| {
                        let x = bx * block + cell % block;
                        let y = by * block + cell / block;
                        f64::from(thresholds[y * BLUE_NOISE_SIZE + x])
                    })
                    .sum();
                (sum / 64.0 - 0.5).abs()
            })
            .fold(0.0, f64::max);
        let horizontal_neighbours = (0..CELLS)
            .filter(|cell| {
                let next = cell - cell % BLUE_NOISE_SIZE + (cell + 1) % BLUE_NOISE_SIZE;
                ranks[*cell].abs_diff(ranks[next]) < CELLS / 64
            })
            .count();

        assert!(sorted.iter().copied().eq(0..CELLS));
        assert!(worst_block < 0.04, "{worst_block}");
        assert!(
            horizontal_neighbours < CELLS / 128,
            "{horizontal_neighbours}"
        );
    }
}
