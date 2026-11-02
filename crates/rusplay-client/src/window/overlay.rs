mod font;

use font::{GLYPH_HEIGHT, GLYPH_WIDTH, glyph, lit};
use rusplay_core::Resolution;

use crate::window::compose::Placement;

pub const MAX_OVERLAY_LINES: usize = 16;
pub const MAX_OVERLAY_COLUMNS: usize = 96;

const CELL_WIDTH: usize = GLYPH_WIDTH + 1;
const CELL_HEIGHT: usize = GLYPH_HEIGHT + 1;
const PADDING: usize = 2;
const MARGIN: usize = 4;
const REFERENCE_HEIGHT: u32 = 540;
const INK: u8 = 255;
const OVERLAY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OverlayText {
    lines: Vec<String>,
}

impl OverlayText {
    pub fn new<I, L>(lines: I) -> Self
    where
        I: IntoIterator<Item = L>,
        L: AsRef<str>,
    {
        Self {
            lines: lines
                .into_iter()
                .take(MAX_OVERLAY_LINES)
                .map(|line| line.as_ref().chars().take(MAX_OVERLAY_COLUMNS).collect())
                .collect(),
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn is_empty(&self) -> bool {
        self.columns() == 0
    }

    fn columns(&self) -> usize {
        self.lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mask {
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

pub(crate) fn scale_for(target: Resolution) -> u32 {
    (target.height / REFERENCE_HEIGHT).max(1)
}

pub(crate) fn origin_for(scale: u32) -> [i32; 2] {
    let margin = i32::try_from(MARGIN).unwrap_or(0) * i32::try_from(scale).unwrap_or(i32::MAX);

    [margin, margin]
}

pub(crate) fn rasterize(text: &OverlayText, scale: u32) -> Option<Mask> {
    if text.is_empty() {
        return None;
    }

    let scale = usize::try_from(scale).ok()?;
    let columns = PADDING * 2 + text.columns() * CELL_WIDTH;
    let rows = PADDING * 2 + text.lines.len() * CELL_HEIGHT;
    let (width, height) = (columns * scale, rows * scale);
    let mut coverage = vec![0u8; width * height];

    for (line_index, line) in text.lines.iter().enumerate() {
        for (column_index, character) in line.chars().enumerate() {
            let shape = glyph(character);
            for (row, bits) in shape.iter().enumerate() {
                for column in (0..GLYPH_WIDTH).filter(|column| lit(*bits, *column)) {
                    let x = (PADDING + column_index * CELL_WIDTH + column) * scale;
                    let y = (PADDING + line_index * CELL_HEIGHT + row) * scale;
                    for dy in 0..scale {
                        let start = (y + dy) * width + x;
                        coverage[start..start + scale].fill(INK);
                    }
                }
            }
        }
    }

    Some(Mask {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        coverage,
    })
}

#[derive(Debug)]
struct Uploaded {
    text: OverlayText,
    scale: u32,
    view: wgpu::TextureView,
    size: [u32; 2],
}

#[derive(Debug, Default)]
pub(crate) struct OverlayLayer {
    uploaded: Option<Uploaded>,
}

impl OverlayLayer {
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        text: Option<&OverlayText>,
        target: Resolution,
    ) -> Option<(wgpu::TextureView, Placement)> {
        let Some(text) = text else {
            self.uploaded = None;
            return None;
        };
        let scale = scale_for(target);

        let current = self
            .uploaded
            .as_ref()
            .is_some_and(|uploaded| uploaded.scale == scale && uploaded.text == *text);
        if !current {
            self.uploaded = upload(device, queue, text, scale);
        }

        self.uploaded.as_ref().map(|uploaded| {
            (
                uploaded.view.clone(),
                Placement {
                    origin: origin_for(scale),
                    size: uploaded.size,
                },
            )
        })
    }
}

fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    text: &OverlayText,
    scale: u32,
) -> Option<Uploaded> {
    let mask = rasterize(text, scale)?;
    let limit = device.limits().max_texture_dimension_2d;
    if mask.width > limit || mask.height > limit {
        return None;
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rusplay-overlay"),
        size: wgpu::Extent3d {
            width: mask.width,
            height: mask.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OVERLAY_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &mask.coverage,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(mask.width),
            rows_per_image: Some(mask.height),
        },
        texture.size(),
    );

    Some(Uploaded {
        text: text.clone(),
        scale,
        view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        size: [mask.width, mask.height],
    })
}

#[cfg(test)]
mod tests;
