use rusplay_core::ColorPrimaries;
use rusplay_scale::ColorVolume;

use super::*;
use crate::{
    Gpu, testing,
    window::{
        compose::Composer,
        output::{DisplayEncoding, Signal},
    },
};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const GREY: u8 = 64;
const PLATE: u8 = 29;
const WHITE: u8 = 255;

fn lit_at(mask: &Mask, x: u32, y: u32) -> bool {
    mask.coverage[(y * mask.width + x) as usize] == INK
}

fn target() -> Resolution {
    Resolution::new(WIDTH, HEIGHT)
}

fn linear_encoding() -> DisplayEncoding {
    DisplayEncoding {
        target: ColorVolume::srgb_display(),
        primaries: ColorPrimaries::Bt709,
        signal: Signal::Linear,
        dither: None,
    }
}

fn composed(gpu: &Gpu, text: Option<&OverlayText>) -> (Vec<u8>, Option<Placement>) {
    let picture = testing::upload(gpu, WIDTH, HEIGHT, &vec![[0.25, 0.25, 0.25, 1.0]; 64 * 48])
        .create_view(&wgpu::TextureViewDescriptor::default());
    let output = testing::texture(
        gpu,
        WIDTH,
        HEIGHT,
        FORMAT,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
    let mut composer = Composer::new(&gpu.device, &gpu.queue, &linear_encoding());
    let mut layer = OverlayLayer::default();

    let overlay = layer.prepare(&gpu.device, &gpu.queue, text, target());
    testing::submit(gpu, |encoder| {
        composer.encode(
            &gpu.device,
            &gpu.queue,
            encoder,
            &picture,
            None,
            overlay.as_ref().map(|(view, placement)| (view, *placement)),
            &output_view,
            FORMAT,
        );
    });

    (
        testing::read_bytes(gpu, &output),
        overlay.map(|(_, placement)| placement),
    )
}

fn red(bytes: &[u8], x: u32, y: u32) -> u8 {
    bytes[((y * WIDTH + x) * 4) as usize]
}

fn near(found: u8, expected: u8) -> bool {
    found.abs_diff(expected) <= 1
}

#[test]
fn a_glyph_is_rasterized_into_its_cell_at_the_requested_scale() {
    let text = OverlayText::new(["I"]);

    let single = rasterize(&text, 1).unwrap();
    let doubled = rasterize(&text, 2).unwrap();

    assert_eq!((single.width, single.height), (10, 12));
    assert_eq!((doubled.width, doubled.height), (20, 24));
    assert!(lit_at(&single, 3, 2));
    assert!(!lit_at(&single, 0, 0));
    assert!(!lit_at(&single, 2, 2));
    assert!(lit_at(&doubled, 6, 4));
    assert!(lit_at(&doubled, 7, 5));
    assert!(!lit_at(&doubled, 5, 4));
}

#[test]
fn lowercase_text_draws_the_uppercase_glyphs_and_unknown_characters_a_question_mark() {
    let lower = rasterize(&OverlayText::new(["ms"]), 1).unwrap();
    let upper = rasterize(&OverlayText::new(["MS"]), 1).unwrap();
    let unknown = rasterize(&OverlayText::new(["\u{263a}"]), 1).unwrap();
    let question = rasterize(&OverlayText::new(["?"]), 1).unwrap();

    assert_eq!(lower, upper);
    assert_eq!(unknown, question);
}

#[test]
fn text_is_bounded_and_blank_text_has_no_mask() {
    let long = OverlayText::new(vec![
        "A".repeat(MAX_OVERLAY_COLUMNS + 10);
        MAX_OVERLAY_LINES + 4
    ]);

    assert_eq!(long.lines().len(), MAX_OVERLAY_LINES);
    assert!(
        long.lines()
            .iter()
            .all(|line| line.len() == MAX_OVERLAY_COLUMNS)
    );
    assert!(rasterize(&OverlayText::new(Vec::<String>::new()), 1).is_none());
    assert!(rasterize(&OverlayText::new([""]), 1).is_none());
}

#[test]
fn the_scale_follows_the_target_height() {
    assert_eq!(scale_for(Resolution::new(640, 360)), 1);
    assert_eq!(scale_for(Resolution::new(1920, 1080)), 2);
    assert_eq!(scale_for(Resolution::new(3840, 2160)), 4);
}

#[test]
fn the_overlay_is_drawn_inside_its_rectangle_only() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let text = OverlayText::new(["I"]);

    let (bytes, placement) = composed(&gpu, Some(&text));

    let placement = placement.unwrap();
    assert_eq!(placement.origin, [4, 4]);
    assert_eq!(placement.size, [10, 12]);
    assert!(near(red(&bytes, 8, 6), WHITE));
    assert!(near(red(&bytes, 4, 4), PLATE));
    assert!(near(red(&bytes, 13, 15), PLATE));
    for (x, y) in [(3, 4), (14, 4), (4, 3), (4, 16), (30, 30), (63, 47)] {
        assert!(near(red(&bytes, x, y), GREY), "({x}, {y})");
    }
}

#[test]
fn nothing_is_drawn_without_an_overlay() {
    let Some(gpu) = testing::gpu() else {
        return;
    };

    let (bytes, placement) = composed(&gpu, None);

    assert!(placement.is_none());
    assert!(
        bytes
            .chunks(4)
            .all(|texel| near(texel[0], GREY) && near(texel[1], GREY))
    );
}

#[test]
fn blank_text_draws_nothing() {
    let Some(gpu) = testing::gpu() else {
        return;
    };
    let text = OverlayText::new([""]);

    let (bytes, placement) = composed(&gpu, Some(&text));

    assert!(placement.is_none());
    assert!(bytes.chunks(4).all(|texel| near(texel[0], GREY)));
}
