use rusplay_core::Resolution;
use rusplay_input::capture::StreamRegion;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalArea {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowLayout {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub pixels: Resolution,
}

impl WindowLayout {
    pub fn letterbox(area: LogicalArea, scale: f32, source: Resolution) -> Option<Self> {
        let left = area.x.ceil();
        let top = area.y.ceil();
        let available_width = (area.x + area.width).floor() - left;
        let available_height = (area.y + area.height).floor() - top;

        let usable = scale.is_finite()
            && scale > 0.0
            && available_width >= 1.0
            && available_height >= 1.0
            && source.pixel_count() > 0;
        if !usable {
            return None;
        }

        let available = (
            float_to_u32(available_width),
            float_to_u32(available_height),
        );
        let (width, height) = fit(available, (source.width, source.height));

        Some(Self {
            x: float_to_i32(left) + offset(available.0, width),
            y: float_to_i32(top) + offset(available.1, height),
            width,
            height,
            pixels: Resolution::new(device_pixels(width, scale), device_pixels(height, scale)),
        })
    }

    pub(crate) fn region(self) -> StreamRegion {
        StreamRegion {
            x: f64::from(self.x),
            y: f64::from(self.y),
            width: f64::from(self.width),
            height: f64::from(self.height),
        }
    }
}

fn fit(available: (u32, u32), source: (u32, u32)) -> (u32, u32) {
    let (available_width, available_height) = (u64::from(available.0), u64::from(available.1));
    let (source_width, source_height) = (u64::from(source.0), u64::from(source.1));

    if available_width * source_height <= available_height * source_width {
        let height = (available_width * source_height + source_width / 2) / source_width;
        (available.0, clamp_dimension(height, available.1))
    } else {
        let width = (available_height * source_width + source_height / 2) / source_height;
        (clamp_dimension(width, available.0), available.1)
    }
}

fn clamp_dimension(value: u64, limit: u32) -> u32 {
    u32::try_from(value).unwrap_or(limit).clamp(1, limit)
}

fn offset(available: u32, used: u32) -> i32 {
    i32::try_from((available - used) / 2).unwrap_or(i32::MAX)
}

fn device_pixels(logical: u32, scale: f32) -> u32 {
    let pixels = (f64::from(logical) * f64::from(scale)).round();

    float_to_u32_f64(pixels).max(1)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn float_to_u32(value: f32) -> u32 {
    f64::from(value).clamp(0.0, f64::from(u32::MAX)) as u32
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn float_to_u32_f64(value: f64) -> u32 {
    value.clamp(0.0, f64::from(u32::MAX)) as u32
}

#[allow(clippy::cast_possible_truncation)]
fn float_to_i32(value: f32) -> i32 {
    f64::from(value).clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: Resolution = Resolution::new(1920, 1080);

    fn area(x: f32, y: f32, width: f32, height: f32) -> LogicalArea {
        LogicalArea {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn an_area_of_the_same_aspect_is_filled() {
        let layout = WindowLayout::letterbox(area(0.0, 40.0, 1280.0, 720.0), 1.0, HD).unwrap();

        assert_eq!((layout.x, layout.y), (0, 40));
        assert_eq!((layout.width, layout.height), (1280, 720));
        assert_eq!(layout.pixels, Resolution::new(1280, 720));
    }

    #[test]
    fn a_taller_area_gets_bars_above_and_below() {
        let layout = WindowLayout::letterbox(area(0.0, 0.0, 1100.0, 700.0), 1.0, HD).unwrap();

        assert_eq!((layout.width, layout.height), (1100, 619));
        assert_eq!((layout.x, layout.y), (0, 40));
    }

    #[test]
    fn a_wider_area_gets_bars_left_and_right() {
        let layout = WindowLayout::letterbox(area(10.0, 0.0, 1000.0, 360.0), 1.0, HD).unwrap();

        assert_eq!((layout.width, layout.height), (640, 360));
        assert_eq!((layout.x, layout.y), (190, 0));
    }

    #[test]
    fn fractional_edges_shrink_to_whole_logical_pixels() {
        let layout = WindowLayout::letterbox(area(0.5, 0.25, 640.0, 360.0), 1.0, HD).unwrap();

        assert_eq!((layout.x, layout.y), (1, 1));
        assert_eq!((layout.width, layout.height), (638, 359));
    }

    #[test]
    fn the_device_size_follows_the_scale_factor() {
        let layout = WindowLayout::letterbox(area(0.0, 0.0, 1280.0, 720.0), 1.25, HD).unwrap();

        assert_eq!(layout.pixels, Resolution::new(1600, 900));
    }

    #[test]
    fn nothing_fits_an_empty_area_or_source() {
        assert_eq!(
            WindowLayout::letterbox(area(0.0, 0.0, 0.5, 100.0), 1.0, HD),
            None
        );
        assert_eq!(
            WindowLayout::letterbox(area(0.0, 0.0, 100.0, 100.0), 0.0, HD),
            None
        );
        assert_eq!(
            WindowLayout::letterbox(area(0.0, 0.0, 100.0, 100.0), 1.0, Resolution::new(0, 0)),
            None
        );
    }

    #[test]
    fn the_input_region_is_the_logical_rectangle() {
        let layout = WindowLayout::letterbox(area(0.0, 0.0, 1100.0, 700.0), 2.0, HD).unwrap();

        let region = layout.region();

        assert_eq!(
            (region.x, region.y, region.width, region.height),
            (0.0, 40.0, 1100.0, 619.0)
        );
    }

    #[test]
    fn an_extreme_aspect_keeps_one_pixel() {
        let layout = WindowLayout::letterbox(
            area(0.0, 0.0, 100.0, 100.0),
            1.0,
            Resolution::new(10_000, 1),
        )
        .unwrap();

        assert_eq!((layout.width, layout.height), (100, 1));
    }
}
