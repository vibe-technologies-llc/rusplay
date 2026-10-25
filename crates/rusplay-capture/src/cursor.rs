const BYTES_PER_PIXEL: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorPixelOrder {
    Rgba,
    Bgra,
    Argb,
    Abgr,
}

impl CursorPixelOrder {
    const fn rgba_indices(self) -> [usize; 4] {
        match self {
            Self::Rgba => [0, 1, 2, 3],
            Self::Bgra => [2, 1, 0, 3],
            Self::Argb => [1, 2, 3, 0],
            Self::Abgr => [3, 2, 1, 0],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorBitmap {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPixels<'a> {
    pub order: CursorPixelOrder,
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: &'a [u8],
}

impl CursorBitmap {
    pub fn from_pixels(pixels: CursorPixels<'_>, hotspot: (i32, i32)) -> Option<Self> {
        let width = usize::try_from(pixels.width).ok()?;
        let height = usize::try_from(pixels.height).ok()?;
        let row_bytes = width.checked_mul(BYTES_PER_PIXEL)?;
        if pixels.stride < row_bytes {
            return None;
        }

        let indices = pixels.order.rgba_indices();
        let mut rgba = Vec::with_capacity(row_bytes.checked_mul(height)?);
        for row in 0..height {
            let start = row.checked_mul(pixels.stride)?;
            let source = pixels.data.get(start..start.checked_add(row_bytes)?)?;
            for pixel in source.as_chunks::<BYTES_PER_PIXEL>().0 {
                rgba.extend(indices.map(|index| pixel[index]));
            }
        }

        Some(Self {
            width: pixels.width,
            height: pixels.height,
            hotspot_x: hotspot.0,
            hotspot_y: hotspot.1,
            rgba,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorShape {
    Unchanged,
    Hidden,
    Bitmap(CursorBitmap),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorObservation {
    pub x: i32,
    pub y: i32,
    pub shape: CursorShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorUpdate {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub bitmap: Option<CursorBitmap>,
}

impl CursorUpdate {
    #[must_use]
    pub fn absorb(mut self, stale: Self) -> Self {
        if self.bitmap.is_none() {
            self.bitmap = stale.bitmap;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorTracker {
    position: Option<(i32, i32)>,
    visible: bool,
    bitmap: Option<CursorBitmap>,
}

impl Default for CursorTracker {
    fn default() -> Self {
        Self {
            position: None,
            visible: true,
            bitmap: None,
        }
    }
}

impl CursorTracker {
    pub fn observe(&mut self, observation: CursorObservation) -> Option<CursorUpdate> {
        let position = (observation.x, observation.y);
        let moved = self.position != Some(position);
        self.position = Some(position);

        let was_visible = self.visible;
        let bitmap = match observation.shape {
            CursorShape::Unchanged => None,
            CursorShape::Hidden => {
                self.visible = false;
                None
            }
            CursorShape::Bitmap(bitmap) => {
                self.visible = true;
                (self.bitmap.as_ref() != Some(&bitmap)).then(|| {
                    self.bitmap = Some(bitmap.clone());
                    bitmap
                })
            }
        };

        (moved || bitmap.is_some() || was_visible != self.visible).then_some(CursorUpdate {
            x: observation.x,
            y: observation.y,
            visible: self.visible,
            bitmap,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(fill: u8) -> CursorBitmap {
        CursorBitmap {
            width: 1,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            rgba: vec![fill; 4],
        }
    }

    fn observation(x: i32, shape: CursorShape) -> CursorObservation {
        CursorObservation { x, y: 7, shape }
    }

    #[test]
    fn bgra_pixels_are_reordered_and_stride_padding_dropped() {
        let data = [
            1, 2, 3, 4, 5, 6, 7, 8, 0xEE, 0xEE, 9, 10, 11, 12, 13, 14, 15, 16, 0xEE, 0xEE,
        ];

        let bitmap = CursorBitmap::from_pixels(
            CursorPixels {
                order: CursorPixelOrder::Bgra,
                width: 2,
                height: 2,
                stride: 10,
                data: &data,
            },
            (1, 1),
        )
        .unwrap();

        assert_eq!(
            bitmap.rgba,
            [3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]
        );
        assert_eq!((bitmap.hotspot_x, bitmap.hotspot_y), (1, 1));
    }

    #[test]
    fn argb_and_abgr_put_alpha_last() {
        let pixel = |order| {
            CursorBitmap::from_pixels(
                CursorPixels {
                    order,
                    width: 1,
                    height: 1,
                    stride: 4,
                    data: &[1, 2, 3, 4],
                },
                (0, 0),
            )
            .unwrap()
            .rgba
        };

        assert_eq!(pixel(CursorPixelOrder::Argb), [2, 3, 4, 1]);
        assert_eq!(pixel(CursorPixelOrder::Abgr), [4, 3, 2, 1]);
        assert_eq!(pixel(CursorPixelOrder::Rgba), [1, 2, 3, 4]);
    }

    #[test]
    fn truncated_pixels_are_rejected() {
        let pixels = CursorPixels {
            order: CursorPixelOrder::Rgba,
            width: 2,
            height: 2,
            stride: 8,
            data: &[0; 12],
        };

        assert_eq!(CursorBitmap::from_pixels(pixels, (0, 0)), None);
    }

    #[test]
    fn bitmap_is_sent_only_when_it_changes() {
        let mut tracker = CursorTracker::default();

        let first = tracker.observe(observation(1, CursorShape::Bitmap(bitmap(1))));
        let same = tracker.observe(observation(2, CursorShape::Bitmap(bitmap(1))));
        let changed = tracker.observe(observation(2, CursorShape::Bitmap(bitmap(2))));

        assert_eq!(first.unwrap().bitmap, Some(bitmap(1)));
        assert_eq!(same.unwrap().bitmap, None);
        assert_eq!(changed.unwrap().bitmap, Some(bitmap(2)));
    }

    #[test]
    fn unchanged_cursor_produces_no_update() {
        let mut tracker = CursorTracker::default();

        tracker.observe(observation(1, CursorShape::Bitmap(bitmap(1))));
        let repeat = tracker.observe(observation(1, CursorShape::Unchanged));

        assert_eq!(repeat, None);
    }

    #[test]
    fn hiding_and_showing_are_reported() {
        let mut tracker = CursorTracker::default();
        tracker.observe(observation(1, CursorShape::Bitmap(bitmap(1))));

        let hidden = tracker
            .observe(observation(1, CursorShape::Hidden))
            .unwrap();
        let shown = tracker
            .observe(observation(1, CursorShape::Bitmap(bitmap(1))))
            .unwrap();

        assert!(!hidden.visible);
        assert!(shown.visible);
        assert_eq!(shown.bitmap, None);
    }

    #[test]
    fn absorbing_keeps_an_unsent_bitmap() {
        let stale = CursorUpdate {
            x: 1,
            y: 1,
            visible: true,
            bitmap: Some(bitmap(3)),
        };
        let fresh = CursorUpdate {
            x: 2,
            y: 2,
            visible: true,
            bitmap: None,
        };

        let merged = fresh.absorb(stale);

        assert_eq!((merged.x, merged.y), (2, 2));
        assert_eq!(merged.bitmap, Some(bitmap(3)));
    }
}
