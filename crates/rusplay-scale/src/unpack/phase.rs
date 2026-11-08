use rusplay_core::ChromaSubsampling;

use crate::{
    Kernel, RgbConvertSettings,
    polar::{PolarGeometry, polar_footprint, sample_anchor},
};

const PHASE_COLUMNS: u32 = 2;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PhaseKernels {
    pub geometry: PolarGeometry,
    pub phase_rows: u32,
    pub taps_per_phase: u32,
    pub offsets: Vec<i32>,
    pub weights: Vec<f32>,
    pub tile_offset: (i32, i32),
    pub tile_size: (u32, u32),
}

impl PhaseKernels {
    pub(crate) fn build(settings: &RgbConvertSettings, block: (u32, u32)) -> Self {
        let geometry = settings.polar_chroma_geometry();
        let kernel = Kernel::new(settings.chroma_filter);
        let phase_rows = match settings.format.chroma {
            ChromaSubsampling::Yuv420 => 2,
            ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444 => 1,
        };

        let phases: Vec<Vec<((i64, i64), f32)>> = (0..phase_rows)
            .flat_map(|y| (0..PHASE_COLUMNS).map(move |x| (x, y)))
            .map(|(x, y)| normalized_taps(&geometry, kernel, x, y))
            .collect();
        let width = phases.iter().map(Vec::len).max().unwrap_or(0);
        let padded: Vec<((i64, i64), f32)> = phases
            .iter()
            .flat_map(|taps| {
                taps.iter()
                    .copied()
                    .chain(std::iter::repeat_n(((0, 0), 0.0), width - taps.len()))
            })
            .collect();
        let (tile_offset, tile_size) = tile_extent(&geometry, &phases, phase_rows, block);

        Self {
            geometry,
            phase_rows,
            taps_per_phase: u32::try_from(width).unwrap_or(u32::MAX),
            offsets: padded
                .iter()
                .flat_map(|&((column, row), _)| [narrow(column), narrow(row)])
                .collect(),
            weights: padded.iter().map(|&(_, weight)| weight).collect(),
            tile_offset,
            tile_size,
        }
    }
}

fn normalized_taps(
    geometry: &PolarGeometry,
    kernel: Kernel,
    x: u32,
    y: u32,
) -> Vec<((i64, i64), f32)> {
    let footprint = polar_footprint(geometry, kernel, x, y);
    let total: f64 = footprint.taps.iter().map(|tap| tap.2).sum();

    footprint
        .taps
        .iter()
        .map(|&(column, row, weight)| {
            let offset = (column - footprint.anchor.0, row - footprint.anchor.1);

            #[allow(clippy::cast_possible_truncation)]
            let normalized = (weight / total) as f32;
            (offset, normalized)
        })
        .collect()
}

fn tile_extent(
    geometry: &PolarGeometry,
    phases: &[Vec<((i64, i64), f32)>],
    phase_rows: u32,
    block: (u32, u32),
) -> ((i32, i32), (u32, u32)) {
    let origin = sample_anchor(geometry, 0, 0);

    let positions: Vec<(i64, i64)> = (0..block.1)
        .flat_map(|y| (0..block.0).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let anchor = sample_anchor(geometry, x, y);
            let phase = (x % PHASE_COLUMNS + PHASE_COLUMNS * (y % phase_rows)) as usize;
            phases[phase]
                .iter()
                .map(move |&((column, row), _)| (anchor.0 + column, anchor.1 + row))
        })
        .collect();
    let low = positions
        .iter()
        .copied()
        .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1)))
        .unwrap_or(origin);
    let high = positions
        .iter()
        .copied()
        .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)))
        .unwrap_or(origin);

    let size = |low: i64, high: i64| u32::try_from(high - low + 1).unwrap_or(u32::MAX);
    (
        (narrow(low.0 - origin.0), narrow(low.1 - origin.1)),
        (size(low.0, high.0), size(low.1, high.1)),
    )
}

fn narrow(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}
