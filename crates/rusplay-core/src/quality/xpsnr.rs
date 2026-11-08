use std::ops::Range;

use super::{PLANES, SamplePicture, SamplePlane, TemporalOrder};
use crate::ChromaSubsampling;

const UHD_SAMPLES: f64 = 3840.0 * 2160.0;
const BLOCK_SCALE: f64 = 32.0;
const BLOCK_MULTIPLE: usize = 4;
const TEMPORAL_GAIN: f64 = 2.0;
const DOWNSAMPLE_ABOVE_SAMPLES: usize = 2048 * 1152;
const SMOOTHING_UP_TO_SAMPLES: usize = 640 * 480;
const MIN_RATIO: f64 = 0.000_01;
const UNMEASURED_ACTIVITY: f64 = 1.0;
const NARROWEST_DOWNSAMPLED_SPAN: usize = 12;

#[derive(Debug, Clone)]
struct Block {
    columns: Range<usize>,
    rows: Range<usize>,
}

struct Grid {
    size: usize,
    columns: usize,
    blocks: Vec<Block>,
}

impl Grid {
    fn new(width: usize, height: usize, size: usize) -> Self {
        let columns = width.div_ceil(size);
        let mut blocks = Vec::with_capacity(columns * height.div_ceil(size));
        for top in (0..height).step_by(size) {
            for left in (0..width).step_by(size) {
                blocks.push(Block {
                    columns: left..(left + size).min(width),
                    rows: top..(top + size).min(height),
                });
            }
        }

        Self {
            size,
            columns,
            blocks,
        }
    }
}

struct Activity<'a> {
    source: &'a SamplePlane,
    previous: Option<&'a SamplePlane>,
    before: Option<&'a SamplePlane>,
    downsample: bool,
    floor: f64,
}

impl Activity<'_> {
    fn of(&self, block: &Block) -> f64 {
        let edge = if self.downsample { 2 } else { 1 };
        let width = self.source.width();
        let height = self.source.height();

        let left = block.columns.start + usize::from(block.columns.start == 0) * edge;
        let right = if block.columns.end < width {
            block.columns.end
        } else {
            block.columns.end.saturating_sub(edge)
        };
        let top = block.rows.start + usize::from(block.rows.start == 0) * edge;
        let bottom = if block.rows.end < height {
            block.rows.end
        } else {
            block.rows.end.saturating_sub(edge)
        };
        if right <= left || bottom <= top {
            return UNMEASURED_ACTIVITY;
        }

        let spatial = if self.downsample {
            if right - block.columns.start > NARROWEST_DOWNSAMPLED_SPAN {
                self.downsampled_highpass(left..right, top..bottom)
            } else {
                0
            }
        } else {
            self.highpass(left..right, top..bottom)
        };
        #[expect(clippy::cast_precision_loss)]
        let spatial_mean = spatial as f64 / ((right - left) * (bottom - top)) as f64;

        let temporal = match (self.previous, self.before) {
            (Some(previous), Some(before)) => self.second_difference(block, previous, before),
            (Some(previous), None) => self.first_difference(block, previous),
            _ => 0,
        };
        #[expect(clippy::cast_precision_loss)]
        let temporal_mean =
            TEMPORAL_GAIN * temporal as f64 / (block.columns.len() * block.rows.len()) as f64;

        (spatial_mean + temporal_mean).max(self.floor)
    }

    fn highpass(&self, columns: Range<usize>, rows: Range<usize>) -> u64 {
        let plane = self.source;
        let mut total = 0;
        for y in rows {
            let above = plane.row(y - 1);
            let row = plane.row(y);
            let below = plane.row(y + 1);
            for x in columns.clone() {
                let centre = 12 * i32::from(row[x]);
                let cross = 2
                    * (i32::from(row[x - 1])
                        + i32::from(row[x + 1])
                        + i32::from(above[x])
                        + i32::from(below[x]));
                let corners = i32::from(above[x - 1])
                    + i32::from(above[x + 1])
                    + i32::from(below[x - 1])
                    + i32::from(below[x + 1]);
                total += u64::from((centre - cross - corners).unsigned_abs());
            }
        }

        total
    }

    fn downsampled_highpass(&self, columns: Range<usize>, rows: Range<usize>) -> u64 {
        let plane = self.source;
        let at = |x: isize, y: isize| clamped(plane, x, y);
        let mut total = 0;
        for y in rows.step_by(2) {
            for x in columns.clone().step_by(2) {
                let (x, y) = (signed(x), signed(y));
                let centre = at(x, y) + at(x + 1, y) + at(x, y + 1) + at(x + 1, y + 1);
                let vertical = at(x, y - 1) + at(x + 1, y - 1) + at(x, y + 2) + at(x + 1, y + 2);
                let horizontal = at(x - 1, y) + at(x + 2, y) + at(x - 1, y + 1) + at(x + 2, y + 1);
                let corners =
                    at(x - 1, y - 1) + at(x + 2, y - 1) + at(x - 1, y + 2) + at(x + 2, y + 2);
                let rim = at(x - 1, y - 2)
                    + at(x, y - 2)
                    + at(x + 1, y - 2)
                    + at(x + 2, y - 2)
                    + at(x - 1, y + 3)
                    + at(x, y + 3)
                    + at(x + 1, y + 3)
                    + at(x + 2, y + 3)
                    + at(x - 2, y - 1)
                    + at(x - 2, y)
                    + at(x - 2, y + 1)
                    + at(x - 2, y + 2)
                    + at(x + 3, y - 1)
                    + at(x + 3, y)
                    + at(x + 3, y + 1)
                    + at(x + 3, y + 2);
                let filtered = 12 * centre - 3 * vertical - 3 * horizontal - 2 * corners - rim;
                total += u64::from(filtered.unsigned_abs());
            }
        }

        total
    }

    fn first_difference(&self, block: &Block, previous: &SamplePlane) -> u64 {
        self.temporal(block, |plane, x, y| {
            self.gathered(plane, x, y) - self.gathered(previous, x, y)
        })
    }

    fn second_difference(
        &self,
        block: &Block,
        previous: &SamplePlane,
        before: &SamplePlane,
    ) -> u64 {
        self.temporal(block, |plane, x, y| {
            self.gathered(plane, x, y) - 2 * self.gathered(previous, x, y)
                + self.gathered(before, x, y)
        })
    }

    fn temporal(
        &self,
        block: &Block,
        difference: impl Fn(&SamplePlane, isize, isize) -> i32,
    ) -> u64 {
        let step = if self.downsample { 2 } else { 1 };
        let mut total = 0;
        for y in block.rows.clone().step_by(step) {
            for x in block.columns.clone().step_by(step) {
                total += u64::from(difference(self.source, signed(x), signed(y)).unsigned_abs());
            }
        }

        total
    }

    fn gathered(&self, plane: &SamplePlane, x: isize, y: isize) -> i32 {
        if self.downsample {
            clamped(plane, x, y)
                + clamped(plane, x + 1, y)
                + clamped(plane, x, y + 1)
                + clamped(plane, x + 1, y + 1)
        } else {
            clamped(plane, x, y)
        }
    }
}

fn signed(value: usize) -> isize {
    isize::try_from(value).unwrap_or(isize::MAX)
}

fn clamped(plane: &SamplePlane, x: isize, y: isize) -> i32 {
    let x = x.clamp(0, signed(plane.width() - 1)).unsigned_abs();
    let y = y.clamp(0, signed(plane.height() - 1)).unsigned_abs();

    i32::from(plane.samples()[y * plane.width() + x])
}

fn block_size(ratio: f64) -> usize {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let units = (BLOCK_SCALE * ratio.sqrt()).round() as usize;

    units * BLOCK_MULTIPLE
}

fn smooth_minimum(weights: &mut [f64], grid: &Grid) {
    let columns = grid.columns;
    let last = weights.len().saturating_sub(1);
    for index in 0..weights.len() {
        let column = index % columns;
        let mut neighbour = if column == 0 {
            if index > 1 { weights[index - 2] } else { 0.0 }
        } else if column * grid.size > grid.size {
            weights[index - 2].max(weights[index])
        } else {
            weights[index]
        };
        if index > columns {
            neighbour = neighbour.max(weights[index - 1 - columns]);
        }
        if index > 0 && weights[index - 1] > neighbour {
            weights[index - 1] = neighbour;
        }
        if index == last && index > columns {
            let bound = weights[index - 1].max(weights[index - columns]);
            if weights[index] > bound {
                weights[index] = bound;
            }
        }
    }
}

fn block_error(source: &SamplePlane, reconstruction: &SamplePlane, block: &Block) -> f64 {
    let mut total = 0_u64;
    for y in block.rows.clone() {
        let source_row = &source.row(y)[block.columns.clone()];
        let reconstruction_row = &reconstruction.row(y)[block.columns.clone()];
        for (&a, &b) in source_row.iter().zip(reconstruction_row) {
            let difference = u64::from(a.abs_diff(b));
            total += difference * difference;
        }
    }

    #[expect(clippy::cast_precision_loss)]
    let total = total as f64;

    total
}

fn rounded_weighted_error(weighted: f64) -> f64 {
    if weighted <= 0.0 {
        0.0
    } else {
        (weighted + 0.5).floor()
    }
}

fn chroma_block(block: &Block, chroma: ChromaSubsampling, size: (usize, usize)) -> Block {
    let (shift_x, shift_y) = match chroma {
        ChromaSubsampling::Yuv420 => (1, 1),
        ChromaSubsampling::Yuv422 => (1, 0),
        ChromaSubsampling::Yuv444 => (0, 0),
    };
    let scale = |range: &Range<usize>, shift: u32, limit: usize| {
        (range.start >> shift)..range.end.div_ceil(1 << shift).min(limit)
    };

    Block {
        columns: scale(&block.columns, shift_x, size.0),
        rows: scale(&block.rows, shift_y, size.1),
    }
}

pub(super) fn weighted_errors(
    source: &SamplePicture,
    reconstruction: &SamplePicture,
    history: &[&SamplePlane],
    order: TemporalOrder,
) -> [f64; PLANES] {
    let luma = source.luma();
    let samples = luma.width() * luma.height();
    #[expect(clippy::cast_precision_loss)]
    let ratio = samples as f64 / UHD_SAMPLES;
    let size = block_size(ratio);
    if size < BLOCK_MULTIPLE {
        return std::array::from_fn(|plane| {
            super::squared_error(&source.planes[plane], &reconstruction.planes[plane])
        });
    }

    let depth = i32::from(source.format().depth.bits());
    let floor = 2_f64.powi(depth - 6);
    let picture_activity = (16.0 * 2_f64.powi(2 * depth - 9) / ratio.max(MIN_RATIO).sqrt()).sqrt();

    let used = history.len().min(order.history_frames());
    let activity = Activity {
        source: luma,
        previous: history.first().copied().filter(|_| used >= 1),
        before: history.get(1).copied().filter(|_| used >= 2),
        downsample: samples > DOWNSAMPLE_ABOVE_SAMPLES,
        floor,
    };

    let grid = Grid::new(luma.width(), luma.height(), size);
    let mut weights: Vec<f64> = grid
        .blocks
        .iter()
        .map(|block| 1.0 / activity.of(block))
        .collect();
    if samples <= SMOOTHING_UP_TO_SAMPLES {
        smooth_minimum(&mut weights, &grid);
    }

    let chroma = source.format().chroma;
    std::array::from_fn(|plane| {
        let source_plane = &source.planes[plane];
        let reconstruction_plane = &reconstruction.planes[plane];
        let size = (source_plane.width(), source_plane.height());
        let weighted: f64 = grid
            .blocks
            .iter()
            .zip(&weights)
            .map(|(block, weight)| {
                let block = if plane == 0 {
                    block.clone()
                } else {
                    chroma_block(block, chroma, size)
                };
                block_error(source_plane, reconstruction_plane, &block) * weight
            })
            .sum();

        rounded_weighted_error(weighted * picture_activity)
    })
}
