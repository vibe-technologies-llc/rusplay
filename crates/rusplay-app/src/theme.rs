use gpui::{Background, BoxShadow, Rgba, linear_color_stop, linear_gradient, point, px};

const BRAND_ANGLE: f32 = 135.0;

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

pub fn background() -> Rgba {
    rgb8(0x0d, 0x0f, 0x15)
}

pub fn backdrop() -> Rgba {
    rgb8(0x00, 0x00, 0x00)
}

pub fn chrome() -> Rgba {
    rgb8(0x12, 0x15, 0x1d)
}

pub fn surface() -> Rgba {
    rgb8(0x17, 0x1a, 0x24)
}

pub fn surface_raised() -> Rgba {
    rgb8(0x1f, 0x23, 0x30)
}

pub fn surface_hover() -> Rgba {
    rgb8(0x29, 0x2e, 0x3d)
}

pub fn inset() -> Rgba {
    rgb8(0x0f, 0x11, 0x18)
}

pub fn border() -> Rgba {
    rgb8(0x26, 0x2b, 0x38)
}

pub fn border_strong() -> Rgba {
    rgb8(0x38, 0x3e, 0x50)
}

pub fn text() -> Rgba {
    rgb8(0xec, 0xee, 0xf5)
}

pub fn text_muted() -> Rgba {
    rgb8(0x93, 0x9a, 0xb0)
}

pub fn text_subtle() -> Rgba {
    rgb8(0x62, 0x69, 0x7e)
}

pub fn accent() -> Rgba {
    rgb8(0x7c, 0x6c, 0xff)
}

pub fn accent_bright() -> Rgba {
    rgb8(0xa9, 0x9e, 0xff)
}

pub fn accent_secondary() -> Rgba {
    rgb8(0x3e, 0xa8, 0xff)
}

pub fn accent_soft() -> Rgba {
    rgba8(0x7c, 0x6c, 0xff, 0x2e)
}

pub fn accent_text() -> Rgba {
    rgb8(0xff, 0xff, 0xff)
}

pub fn danger() -> Rgba {
    rgb8(0xff, 0x6b, 0x81)
}

pub fn danger_soft() -> Rgba {
    rgba8(0xff, 0x6b, 0x81, 0x24)
}

pub fn success() -> Rgba {
    rgb8(0x4a, 0xde, 0x9a)
}

pub fn success_soft() -> Rgba {
    rgba8(0x4a, 0xde, 0x9a, 0x22)
}

pub fn warning() -> Rgba {
    rgb8(0xf5, 0xb8, 0x4b)
}

pub fn warning_soft() -> Rgba {
    rgba8(0xf5, 0xb8, 0x4b, 0x22)
}

pub fn selection() -> Rgba {
    rgba8(0x7c, 0x6c, 0xff, 0x55)
}

pub fn brand() -> Background {
    linear_gradient(
        BRAND_ANGLE,
        linear_color_stop(accent(), 0.0),
        linear_color_stop(accent_secondary(), 1.0),
    )
}

pub fn brand_hover() -> Background {
    linear_gradient(
        BRAND_ANGLE,
        linear_color_stop(accent_bright(), 0.0),
        linear_color_stop(accent_secondary(), 1.0),
    )
}

pub fn glow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: rgba8(0x7c, 0x6c, 0xff, 0x55).into(),
        offset: point(px(0.0), px(4.0)),
        blur_radius: px(14.0),
        spread_radius: px(-2.0),
        inset: false,
    }]
}

pub fn lift() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: rgba8(0x00, 0x00, 0x00, 0x66).into(),
        offset: point(px(0.0), px(6.0)),
        blur_radius: px(18.0),
        spread_radius: px(-6.0),
        inset: false,
    }]
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
