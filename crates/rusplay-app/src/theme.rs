use gpui::Rgba;

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
    rgb8(0x16, 0x18, 0x1d)
}

pub fn backdrop() -> Rgba {
    rgb8(0x00, 0x00, 0x00)
}

pub fn surface() -> Rgba {
    rgb8(0x1e, 0x21, 0x28)
}

pub fn surface_raised() -> Rgba {
    rgb8(0x28, 0x2c, 0x35)
}

pub fn surface_hover() -> Rgba {
    rgb8(0x32, 0x37, 0x43)
}

pub fn border() -> Rgba {
    rgb8(0x3a, 0x3f, 0x4b)
}

pub fn text() -> Rgba {
    rgb8(0xe6, 0xe8, 0xee)
}

pub fn text_muted() -> Rgba {
    rgb8(0x90, 0x97, 0xa6)
}

pub fn accent() -> Rgba {
    rgb8(0x4f, 0x8c, 0xff)
}

pub fn accent_text() -> Rgba {
    rgb8(0xff, 0xff, 0xff)
}

pub fn danger() -> Rgba {
    rgb8(0xff, 0x6b, 0x6b)
}

pub fn success() -> Rgba {
    rgb8(0x5f, 0xd1, 0x8b)
}

pub fn selection() -> Rgba {
    rgba8(0x4f, 0x8c, 0xff, 0x55)
}
