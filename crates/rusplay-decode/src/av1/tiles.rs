use super::{bits::BitReader, frame::FrameSize, sequence::Sequence};
use crate::Av1SyntaxError;

const MAX_TILE_COLS: u32 = 64;
const MAX_TILE_ROWS: u32 = 64;
const MAX_TILE_WIDTH: u32 = 4096;
const MAX_TILE_AREA: u32 = 4096 * 2304;
const SUPERBLOCK_SIZE_EXTRA_SHIFT: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TileLayout {
    pub(super) cols: u32,
    pub(super) rows: u32,
    cols_log2: u32,
    rows_log2: u32,
    size_bytes: u32,
}

impl TileLayout {
    pub(super) fn tiles(&self) -> u32 {
        self.cols * self.rows
    }
}

pub(super) fn tile_info(
    r: &mut BitReader<'_>,
    sequence: &Sequence,
    size: &FrameSize,
) -> Result<TileLayout, Av1SyntaxError> {
    let shift = sequence.superblock_shift();
    let sb_cols = size.mi_cols().div_ceil(1 << shift);
    let sb_rows = size.mi_rows().div_ceil(1 << shift);
    let sb_size = shift + SUPERBLOCK_SIZE_EXTRA_SHIFT;
    let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
    let max_tile_area_sb = MAX_TILE_AREA >> (2 * sb_size);
    let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
    let max_log2_tile_cols = tile_log2(1, sb_cols.min(MAX_TILE_COLS));
    let max_log2_tile_rows = tile_log2(1, sb_rows.min(MAX_TILE_ROWS));
    let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));

    let (cols, cols_log2, rows, rows_log2) = if r.bit()? {
        let cols_log2 = increments(r, min_log2_tile_cols, max_log2_tile_cols)?;
        let tile_width_sb = sb_cols.div_ceil(1 << cols_log2);
        let cols = sb_cols.div_ceil(tile_width_sb);

        let min_log2_tile_rows = min_log2_tiles.saturating_sub(cols_log2);
        let rows_log2 = increments(r, min_log2_tile_rows, max_log2_tile_rows)?;
        let tile_height_sb = sb_rows.div_ceil(1 << rows_log2);
        let rows = sb_rows.div_ceil(tile_height_sb);

        (cols, cols_log2, rows, rows_log2)
    } else {
        let (cols, widest_tile_sb) =
            explicit_sizes(r, sb_cols, max_tile_width_sb, MAX_TILE_COLS, |count| {
                Av1SyntaxError::TileColumnsOutOfRange(count)
            })?;

        let area = sb_rows * sb_cols;
        let max_tile_area_sb = if min_log2_tiles > 0 {
            area >> (min_log2_tiles + 1)
        } else {
            area
        };
        let max_tile_height_sb = (max_tile_area_sb / widest_tile_sb).max(1);
        let (rows, _) = explicit_sizes(r, sb_rows, max_tile_height_sb, MAX_TILE_ROWS, |count| {
            Av1SyntaxError::TileRowsOutOfRange(count)
        })?;

        (cols, tile_log2(1, cols), rows, tile_log2(1, rows))
    };

    if cols > MAX_TILE_COLS {
        return Err(Av1SyntaxError::TileColumnsOutOfRange(cols));
    }
    if rows > MAX_TILE_ROWS {
        return Err(Av1SyntaxError::TileRowsOutOfRange(rows));
    }

    let mut size_bytes = 0;
    if cols_log2 > 0 || rows_log2 > 0 {
        let tile = r.bits(cols_log2 + rows_log2)?;
        let tiles = cols * rows;
        if tile >= tiles {
            return Err(Av1SyntaxError::ContextUpdateTileOutOfRange { tile, tiles });
        }
        size_bytes = r.bits(2)? + 1;
    }

    Ok(TileLayout {
        cols,
        rows,
        cols_log2,
        rows_log2,
        size_bytes,
    })
}

fn increments(r: &mut BitReader<'_>, minimum: u32, maximum: u32) -> Result<u32, Av1SyntaxError> {
    let mut log2 = minimum;
    while log2 < maximum && r.bit()? {
        log2 += 1;
    }
    Ok(log2)
}

fn explicit_sizes(
    r: &mut BitReader<'_>,
    superblocks: u32,
    max_tile_sb: u32,
    max_tiles: u32,
    too_many: impl Fn(u32) -> Av1SyntaxError,
) -> Result<(u32, u32), Av1SyntaxError> {
    let mut start = 0;
    let mut count = 0;
    let mut largest = 0;

    while start < superblocks {
        if count == max_tiles {
            return Err(too_many(count + 1));
        }
        let size = r.non_symmetric((superblocks - start).min(max_tile_sb))? + 1;
        largest = largest.max(size);
        start += size;
        count += 1;
    }

    Ok((count, largest))
}

fn tile_log2(block: u32, target: u32) -> u32 {
    let mut log2 = 0;
    while u64::from(block) << log2 < u64::from(target) {
        log2 += 1;
    }
    log2
}

pub(super) fn tile_group(
    r: &mut BitReader<'_>,
    layout: &TileLayout,
    expected: u32,
) -> Result<u32, Av1SyntaxError> {
    let tiles = layout.tiles();
    let (start, end) = if tiles > 1 && r.bit()? {
        let bits = layout.cols_log2 + layout.rows_log2;
        (r.bits(bits)?, r.bits(bits)?)
    } else {
        (0, tiles - 1)
    };
    r.byte_align();

    if start != expected || end < start || end >= tiles {
        return Err(Av1SyntaxError::TileGroupOutOfOrder {
            start,
            end,
            expected,
            tiles,
        });
    }

    for tile in start..end {
        let size = r.little_endian(layout.size_bytes)? + 1;
        let remaining = r.remaining_bytes();
        if size > remaining {
            return Err(Av1SyntaxError::TileSizePastData {
                tile,
                size,
                remaining,
            });
        }
        r.skip_bytes(size)?;
    }

    if r.remaining_bytes() == 0 {
        return Err(Av1SyntaxError::EmptyTile { tile: end });
    }

    Ok(end)
}
