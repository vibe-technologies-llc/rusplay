use std::mem;

use pipewire::spa::{param::video::VideoFormat, sys};

use crate::{
    CaptureError,
    cursor::{CursorBitmap, CursorObservation, CursorPixelOrder, CursorPixels, CursorShape},
    pipewire::format::{MetaSize, meta_param, size_as_i32},
};

const CURSOR_SIZE: usize = mem::size_of::<sys::spa_meta_cursor>();
const BITMAP_SIZE: usize = mem::size_of::<sys::spa_meta_bitmap>();
const BYTES_PER_PIXEL: usize = 4;
const DEFAULT_CURSOR_EDGE: usize = 64;
const MAX_CURSOR_EDGE: usize = 512;

const fn meta_size(edge: usize) -> usize {
    CURSOR_SIZE + BITMAP_SIZE + edge * edge * BYTES_PER_PIXEL
}

pub(crate) fn cursor_meta_param() -> Result<Vec<u8>, CaptureError> {
    meta_param(
        sys::SPA_META_Cursor,
        MetaSize::Range {
            default: size_as_i32(meta_size(DEFAULT_CURSOR_EDGE)),
            min: size_as_i32(meta_size(1)),
            max: size_as_i32(meta_size(MAX_CURSOR_EDGE)),
        },
    )
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let field: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
    Some(u32::from_ne_bytes(field))
}

fn read_i32(bytes: &[u8], at: usize) -> Option<i32> {
    read_u32(bytes, at).map(u32::cast_signed)
}

fn pixel_order(format: u32) -> Option<CursorPixelOrder> {
    match VideoFormat(format) {
        VideoFormat::RGBA => Some(CursorPixelOrder::Rgba),
        VideoFormat::BGRA => Some(CursorPixelOrder::Bgra),
        VideoFormat::ARGB => Some(CursorPixelOrder::Argb),
        VideoFormat::ABGR => Some(CursorPixelOrder::Abgr),
        _ => None,
    }
}

pub(crate) fn parse_cursor_meta(meta: &[u8]) -> Option<CursorObservation> {
    if meta.len() < CURSOR_SIZE || read_u32(meta, 0)? == 0 {
        return None;
    }

    let x = read_i32(meta, 8)?;
    let y = read_i32(meta, 12)?;
    let hotspot = (read_i32(meta, 16)?, read_i32(meta, 20)?);
    let bitmap_offset = usize::try_from(read_u32(meta, 24)?).ok()?;

    let shape = if bitmap_offset < CURSOR_SIZE {
        CursorShape::Unchanged
    } else {
        parse_bitmap(meta, bitmap_offset, hotspot)
    };

    Some(CursorObservation { x, y, shape })
}

fn parse_bitmap(meta: &[u8], at: usize, hotspot: (i32, i32)) -> CursorShape {
    let Some(bitmap) = meta.get(at..).filter(|bitmap| bitmap.len() >= BITMAP_SIZE) else {
        return CursorShape::Unchanged;
    };
    let field = |offset| read_u32(bitmap, offset);
    let (Some(format), Some(width), Some(height), Some(stride), Some(pixels_at)) =
        (field(0), field(4), field(8), field(12), field(16))
    else {
        return CursorShape::Unchanged;
    };
    if width == 0 || height == 0 {
        return CursorShape::Hidden;
    }

    let pixels = usize::try_from(pixels_at)
        .ok()
        .filter(|pixels_at| *pixels_at >= BITMAP_SIZE)
        .and_then(|pixels_at| bitmap.get(pixels_at..));
    let decoded = pixel_order(format)
        .zip(pixels)
        .zip(usize::try_from(stride.cast_signed().unsigned_abs()).ok())
        .and_then(|((order, data), stride)| {
            CursorBitmap::from_pixels(
                CursorPixels {
                    order,
                    width,
                    height,
                    stride,
                    data,
                },
                hotspot,
            )
        });

    decoded.map_or(CursorShape::Unchanged, CursorShape::Bitmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MetaBuilder {
        bytes: Vec<u8>,
    }

    impl MetaBuilder {
        fn cursor(id: u32, position: (i32, i32), hotspot: (i32, i32), bitmap_at: u32) -> Self {
            let mut bytes = Vec::new();
            for word in [
                id,
                0,
                position.0.cast_unsigned(),
                position.1.cast_unsigned(),
                hotspot.0.cast_unsigned(),
                hotspot.1.cast_unsigned(),
                bitmap_at,
            ] {
                bytes.extend(word.to_ne_bytes());
            }
            Self { bytes }
        }

        fn bitmap(mut self, format: VideoFormat, size: (u32, u32), pixels: &[u8]) -> Self {
            let stride = size.0 * 4;
            let pixels_at = u32::try_from(BITMAP_SIZE).unwrap();
            for word in [format.as_raw(), size.0, size.1, stride, pixels_at] {
                self.bytes.extend(word.to_ne_bytes());
            }
            self.bytes.extend_from_slice(pixels);
            self
        }
    }

    fn bitmap_at() -> u32 {
        u32::try_from(CURSOR_SIZE).unwrap()
    }

    #[test]
    fn layout_matches_the_spa_headers() {
        assert_eq!(CURSOR_SIZE, 28);
        assert_eq!(BITMAP_SIZE, 20);
        assert_eq!(mem::offset_of!(sys::spa_meta_cursor, bitmap_offset), 24);
        assert_eq!(mem::offset_of!(sys::spa_meta_bitmap, offset), 16);
    }

    #[test]
    fn cursor_with_bitmap_is_parsed() {
        let meta = MetaBuilder::cursor(1, (100, -5), (3, 4), bitmap_at())
            .bitmap(VideoFormat::BGRA, (1, 1), &[10, 20, 30, 255])
            .bytes;

        let observation = parse_cursor_meta(&meta).unwrap();

        assert_eq!((observation.x, observation.y), (100, -5));
        assert_eq!(
            observation.shape,
            CursorShape::Bitmap(CursorBitmap {
                width: 1,
                height: 1,
                hotspot_x: 3,
                hotspot_y: 4,
                rgba: vec![30, 20, 10, 255],
            })
        );
    }

    #[test]
    fn cursor_without_bitmap_keeps_its_shape() {
        let meta = MetaBuilder::cursor(1, (5, 6), (0, 0), 0).bytes;

        let observation = parse_cursor_meta(&meta).unwrap();

        assert_eq!(observation.shape, CursorShape::Unchanged);
    }

    #[test]
    fn empty_bitmap_hides_the_cursor() {
        let meta = MetaBuilder::cursor(1, (5, 6), (0, 0), bitmap_at())
            .bitmap(VideoFormat::RGBA, (0, 0), &[])
            .bytes;

        let observation = parse_cursor_meta(&meta).unwrap();

        assert_eq!(observation.shape, CursorShape::Hidden);
    }

    #[test]
    fn invalid_cursor_id_means_no_data() {
        let meta = MetaBuilder::cursor(0, (5, 6), (0, 0), 0).bytes;

        assert_eq!(parse_cursor_meta(&meta), None);
        assert_eq!(parse_cursor_meta(&meta[..10]), None);
    }

    #[test]
    fn truncated_bitmap_leaves_the_shape_unchanged() {
        let meta = MetaBuilder::cursor(1, (5, 6), (0, 0), bitmap_at())
            .bitmap(VideoFormat::RGBA, (2, 2), &[0; 8])
            .bytes;

        let observation = parse_cursor_meta(&meta).unwrap();

        assert_eq!(observation.shape, CursorShape::Unchanged);
    }

    #[test]
    fn cursor_meta_param_ranges_over_bitmap_sizes() {
        let bytes = cursor_meta_param().unwrap();

        let object = crate::pipewire::format::parse_object(&bytes).unwrap();

        assert_eq!(object.properties.len(), 2);
    }
}
