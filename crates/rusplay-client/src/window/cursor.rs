use std::sync::Arc;

use half::f16;
use rusplay_config::ScalingConfig;
use rusplay_core::Resolution;
use rusplay_protocol::CursorImage;
use rusplay_scale::{OutputLight, ScalePipelines, Scaler};

use crate::{ClientError, CursorState, present, window::compose::CursorPlacement};

const LINEAR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const LINEAR_TEXEL_BYTES: u32 = 8;
const CHANNEL_MAX: f32 = 255.0;

#[derive(Debug)]
struct Uploaded {
    image: Arc<CursorImage>,
    texture: wgpu::Texture,
}

#[derive(Debug)]
struct Scaled {
    image: Arc<CursorImage>,
    size: Resolution,
    view: wgpu::TextureView,
}

#[derive(Debug)]
pub(crate) struct CursorLayer {
    pipelines: ScalePipelines,
    scaling: ScalingConfig,
    scaler: Option<Scaler>,
    uploaded: Option<Uploaded>,
    scaled: Option<Scaled>,
}

impl CursorLayer {
    pub(crate) fn new(device: &wgpu::Device, scaling: ScalingConfig) -> Self {
        Self {
            pipelines: ScalePipelines::new(device),
            scaling,
            scaler: None,
            uploaded: None,
            scaled: None,
        }
    }

    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        cursor: &CursorState,
        target: Resolution,
    ) -> Result<Option<(wgpu::TextureView, CursorPlacement)>, ClientError> {
        let Some(image) = cursor.image.as_ref().filter(|_| cursor.visible) else {
            return Ok(None);
        };
        let space = cursor.space;
        if image.width == 0 || image.height == 0 || space.pixel_count() == 0 {
            return Ok(None);
        }

        let size = Resolution::new(
            scaled_length(u32::from(image.width), space.width, target.width),
            scaled_length(u32::from(image.height), space.height, target.height),
        );
        let placement = CursorPlacement {
            origin: [
                scaled_offset(
                    cursor.x - i32::from(image.hotspot_x),
                    space.width,
                    target.width,
                ),
                scaled_offset(
                    cursor.y - i32::from(image.hotspot_y),
                    space.height,
                    target.height,
                ),
            ],
            size: [size.width, size.height],
        };

        let current = self
            .scaled
            .as_ref()
            .is_some_and(|scaled| Arc::ptr_eq(&scaled.image, image) && scaled.size == size);
        if !current {
            let view = self.rebuild(device, queue, encoder, image, size)?;
            self.scaled = Some(Scaled {
                image: Arc::clone(image),
                size,
                view,
            });
        }

        Ok(self
            .scaled
            .as_ref()
            .map(|scaled| (scaled.view.clone(), placement)))
    }

    fn rebuild(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        image: &Arc<CursorImage>,
        size: Resolution,
    ) -> Result<wgpu::TextureView, ClientError> {
        let stale = self
            .uploaded
            .as_ref()
            .is_some_and(|uploaded| !Arc::ptr_eq(&uploaded.image, image));
        if stale {
            self.uploaded = None;
        }
        let uploaded = self.uploaded.get_or_insert_with(|| Uploaded {
            image: Arc::clone(image),
            texture: upload(device, queue, image),
        });

        let source = Resolution::new(uploaded.texture.width(), uploaded.texture.height());
        let source_view = uploaded
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if source == size {
            return Ok(source_view);
        }

        let settings = present::scale_settings(&self.scaling, source, size, OutputLight::Linear);
        let scaler = match self.scaler.take() {
            Some(mut scaler) => {
                scaler.configure(device, settings)?;
                scaler
            }
            None => Scaler::new(&self.pipelines, device, settings)?,
        };
        let scaler = self.scaler.insert(scaler);

        let target = linear_texture(
            device,
            "rusplay-cursor-scaled",
            size,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        )
        .create_view(&wgpu::TextureViewDescriptor::default());
        scaler.encode(device, encoder, &source_view, &target);

        Ok(target)
    }
}

fn upload(device: &wgpu::Device, queue: &wgpu::Queue, image: &CursorImage) -> wgpu::Texture {
    let size = Resolution::new(u32::from(image.width), u32::from(image.height));
    let texture = linear_texture(
        device,
        "rusplay-cursor",
        size,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );

    queue.write_texture(
        texture.as_image_copy(),
        &linear_premultiplied(&image.rgba),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.width * LINEAR_TEXEL_BYTES),
            rows_per_image: Some(size.height),
        },
        wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        },
    );

    texture
}

fn linear_texture(
    device: &wgpu::Device,
    label: &str,
    size: Resolution,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: LINEAR_FORMAT,
        usage,
        view_formats: &[],
    })
}

pub(crate) fn linear_premultiplied(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() * 2);

    for pixel in rgba.as_chunks::<4>().0 {
        let alpha = f32::from(pixel[3]) / CHANNEL_MAX;
        let channels = [
            srgb_to_linear(pixel[0]) * alpha,
            srgb_to_linear(pixel[1]) * alpha,
            srgb_to_linear(pixel[2]) * alpha,
            alpha,
        ];
        for channel in channels {
            out.extend_from_slice(&f16::from_f32(channel).to_le_bytes());
        }
    }

    out
}

fn srgb_to_linear(encoded: u8) -> f32 {
    let value = f32::from(encoded) / CHANNEL_MAX;

    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn scaled_length(length: u32, from: u32, to: u32) -> u32 {
    let scaled = (u64::from(length) * u64::from(to) + u64::from(from) / 2) / u64::from(from);

    u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
}

pub(crate) fn scaled_offset(offset: i32, from: u32, to: u32) -> i32 {
    let scaled = (f64::from(offset) * f64::from(to) / f64::from(from)).round();

    #[allow(clippy::cast_possible_truncation)]
    let clamped = scaled.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gpu;

    fn gpu() -> Option<Gpu> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&rusplay_scale::device_descriptor(&adapter)))
                .ok()?;

        Some(Gpu { device, queue })
    }

    fn state(visible: bool) -> CursorState {
        CursorState {
            x: 10,
            y: 20,
            space: Resolution::new(100, 100),
            visible,
            image: Some(Arc::new(CursorImage {
                width: 4,
                height: 4,
                hotspot_x: 1,
                hotspot_y: 1,
                rgba: vec![255; 64],
            })),
        }
    }

    #[test]
    fn the_cursor_follows_the_picture_scale_on_the_gpu() {
        let Some(gpu) = gpu() else {
            return;
        };
        let mut layer = CursorLayer::new(&gpu.device, ScalingConfig::default());
        let cursor = state(true);

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let halved = layer
            .prepare(
                &gpu.device,
                &gpu.queue,
                &mut encoder,
                &cursor,
                Resolution::new(50, 50),
            )
            .unwrap()
            .unwrap();
        let native = layer
            .prepare(
                &gpu.device,
                &gpu.queue,
                &mut encoder,
                &cursor,
                Resolution::new(100, 100),
            )
            .unwrap()
            .unwrap();
        gpu.queue.submit([encoder.finish()]);

        assert_eq!(halved.1.size, [2, 2]);
        assert_eq!(halved.1.origin, [5, 10]);
        assert_eq!(native.1.size, [4, 4]);
        assert_eq!(native.1.origin, [9, 19]);
    }

    #[test]
    fn a_hidden_cursor_or_one_without_a_shape_draws_nothing() {
        let Some(gpu) = gpu() else {
            return;
        };
        let mut layer = CursorLayer::new(&gpu.device, ScalingConfig::default());
        let shapeless = CursorState {
            image: None,
            ..state(true)
        };

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for cursor in [state(false), shapeless] {
            let placed = layer
                .prepare(
                    &gpu.device,
                    &gpu.queue,
                    &mut encoder,
                    &cursor,
                    Resolution::new(50, 50),
                )
                .unwrap();
            assert!(placed.is_none());
        }
    }

    fn f16_at(bytes: &[u8], index: usize) -> f32 {
        f16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]).to_f32()
    }

    #[test]
    fn opaque_white_stays_white_and_transparent_texels_vanish() {
        let bytes = linear_premultiplied(&[255, 255, 255, 255, 255, 0, 0, 0]);

        assert_eq!(bytes.len(), 16);
        for channel in 0..4 {
            assert!((f16_at(&bytes, channel) - 1.0).abs() < 1e-3);
            assert!(f16_at(&bytes, 4 + channel).abs() < 1e-6);
        }
    }

    #[test]
    fn colour_is_linearised_before_it_is_premultiplied() {
        let bytes = linear_premultiplied(&[188, 0, 0, 128]);

        let alpha = 128.0 / 255.0;
        let red = srgb_to_linear(188) * alpha;
        assert!((f16_at(&bytes, 0) - red).abs() < 1e-3);
        assert!((f16_at(&bytes, 3) - alpha).abs() < 1e-3);
        assert!((srgb_to_linear(188) - 0.5).abs() < 0.01);
    }

    #[test]
    fn sizes_and_offsets_follow_the_picture_scale() {
        assert_eq!(scaled_length(32, 1920, 1280), 21);
        assert_eq!(scaled_length(1, 3840, 640), 1);
        assert_eq!(scaled_length(24, 1280, 2560), 48);
        assert_eq!(scaled_offset(-4, 1920, 960), -2);
        assert_eq!(scaled_offset(1919, 1920, 1280), 1279);
    }
}
