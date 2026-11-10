use std::sync::Arc;

use gpui::{BoxShadow, FontFeatures, Pixels, Rgba, point, px};

pub const RADIUS_CONTROL: f32 = 4.0;
pub const RADIUS_PANEL: f32 = 6.0;
pub const SELECTION_BAR: f32 = 2.0;

const QUIET_ALPHA: u8 = 0x24;
const SELECTION_ALPHA: u8 = 0x59;

fn rgb8(red: u8, green: u8, blue: u8) -> Rgba {
    rgba8(red, green, blue, 0xff)
}

fn rgba8(red: u8, green: u8, blue: u8, alpha: u8) -> Rgba {
    Rgba {
        r: f32::from(red) / 255.0,
        g: f32::from(green) / 255.0,
        b: f32::from(blue) / 255.0,
        a: f32::from(alpha) / 255.0,
    }
}

fn quiet(color: Rgba, alpha: u8) -> Rgba {
    Rgba {
        a: f32::from(alpha) / 255.0,
        ..color
    }
}

pub fn radius_control() -> Pixels {
    px(RADIUS_CONTROL)
}

pub fn radius_panel() -> Pixels {
    px(RADIUS_PANEL)
}

pub fn canvas() -> Rgba {
    rgb8(0x16, 0x16, 0x18)
}

pub fn backdrop() -> Rgba {
    rgb8(0x00, 0x00, 0x00)
}

pub fn chrome() -> Rgba {
    rgb8(0x1b, 0x1b, 0x1d)
}

pub fn surface() -> Rgba {
    rgb8(0x20, 0x20, 0x22)
}

pub fn raised() -> Rgba {
    rgb8(0x2a, 0x2a, 0x2d)
}

pub fn raised_hover() -> Rgba {
    rgb8(0x33, 0x33, 0x37)
}

pub fn inset() -> Rgba {
    rgb8(0x11, 0x11, 0x13)
}

pub fn rule() -> Rgba {
    rgb8(0x2f, 0x2f, 0x33)
}

pub fn rule_strong() -> Rgba {
    rgb8(0x3d, 0x3d, 0x42)
}

pub fn text() -> Rgba {
    rgb8(0xe6, 0xe6, 0xe8)
}

pub fn text_muted() -> Rgba {
    rgb8(0xa3, 0xa3, 0xa8)
}

pub fn text_faint() -> Rgba {
    rgb8(0x88, 0x88, 0x8d)
}

pub fn accent() -> Rgba {
    rgb8(0x5b, 0x9b, 0xf0)
}

pub fn accent_hover() -> Rgba {
    rgb8(0x72, 0xaa, 0xf5)
}

pub fn accent_ink() -> Rgba {
    rgb8(0x0a, 0x14, 0x24)
}

pub fn accent_quiet() -> Rgba {
    quiet(accent(), QUIET_ALPHA)
}

pub fn selection() -> Rgba {
    quiet(accent(), SELECTION_ALPHA)
}

pub fn live() -> Rgba {
    rgb8(0x5f, 0xb3, 0x7a)
}

pub fn warning() -> Rgba {
    rgb8(0xd6, 0xa9, 0x3c)
}

pub fn danger() -> Rgba {
    rgb8(0xe6, 0x5e, 0x5e)
}

pub fn danger_quiet() -> Rgba {
    quiet(danger(), QUIET_ALPHA)
}

pub fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".to_owned(), 1)]))
}

pub fn window_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: rgba8(0x00, 0x00, 0x00, 0x80).into(),
        offset: point(px(0.0), px(2.0)),
        blur_radius: px(8.0),
        spread_radius: px(0.0),
        inset: false,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMUM_CONTRAST: f32 = 4.5;

    fn channel(value: f32) -> f32 {
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(color: Rgba) -> f32 {
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    fn contrast(foreground: Rgba, background: Rgba) -> f32 {
        let (first, second) = (luminance(foreground), luminance(background));
        let (light, dark) = if first > second {
            (first, second)
        } else {
            (second, first)
        };

        (light + 0.05) / (dark + 0.05)
    }

    fn assert_readable(name: &str, foreground: Rgba, backgrounds: &[(&str, Rgba)]) {
        for (background_name, background) in backgrounds {
            let ratio = contrast(foreground, *background);
            assert!(
                ratio >= MINIMUM_CONTRAST,
                "{name} on {background_name} is {ratio:.2}:1"
            );
        }
    }

    #[test]
    fn every_text_colour_reads_at_four_and_a_half_to_one_where_it_is_used() {
        let panels = [
            ("canvas", canvas()),
            ("chrome", chrome()),
            ("surface", surface()),
            ("raised", raised()),
        ];
        let pages = [("canvas", canvas()), ("chrome", chrome())];

        assert_readable("text", text(), &panels);
        assert_readable("text_muted", text_muted(), &panels);
        assert_readable("text_faint", text_faint(), &pages);
        assert_readable("accent", accent(), &panels[..3]);
        assert_readable("live", live(), &panels[..3]);
        assert_readable("warning", warning(), &panels[..3]);
        assert_readable("danger", danger(), &panels[..3]);
        assert_readable("accent_ink", accent_ink(), &[("accent", accent())]);
        assert_readable(
            "accent_ink",
            accent_ink(),
            &[("accent_hover", accent_hover())],
        );
    }
}
