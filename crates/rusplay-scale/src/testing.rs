use std::sync::OnceLock;

use half::f16;

pub(crate) const RGBA16F_BYTES: u32 = 8;

pub(crate) struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

pub(crate) fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();

    GPU.get_or_init(|| pollster::block_on(request_gpu()))
        .as_ref()
}

async fn request_gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..wgpu::RequestAdapterOptions::default()
        })
        .await
        .ok()?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .ok()?;

    Some(Gpu { device, queue })
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

impl Image {
    pub fn from_fn(width: u32, height: u32, texel: impl Fn(u32, u32) -> [f32; 4]) -> Self {
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| texel(x, y).map(quantize_f16))
            .collect();

        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn pattern(width: u32, height: u32, seed: u32) -> Self {
        Self::from_fn(width, height, |x, y| {
            let noise = |channel: u32| unit_hash(x, y, seed.wrapping_add(channel));
            let edge = if (x / 5 + y / 7) % 2 == 0 { 0.9 } else { 0.1 };

            [noise(0), edge, 0.5 * noise(1) + 0.25 * edge, 1.0]
        })
    }

    pub fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let clamp = |value: i64, len: u32| {
            usize::try_from(value.clamp(0, i64::from(len) - 1)).unwrap_or_default()
        };

        self.pixels[clamp(y, self.height) * self.width as usize + clamp(x, self.width)]
    }

    pub fn max_difference(&self, other: &Self) -> f32 {
        assert_eq!((self.width, self.height), (other.width, other.height));

        self.pixels
            .iter()
            .zip(&other.pixels)
            .flat_map(|(left, right)| left.iter().zip(right).map(|(a, b)| (a - b).abs()))
            .fold(0.0, f32::max)
    }
}

pub(crate) fn quantize_f16(value: f32) -> f32 {
    f16::from_f32(value).to_f32()
}

fn unit_hash(x: u32, y: u32, seed: u32) -> f32 {
    let mut state = x
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(y.wrapping_mul(0x85EB_CA77))
        .wrapping_add(seed.wrapping_mul(0xC2B2_AE3D));
    state ^= state >> 15;
    state = state.wrapping_mul(0x2C1B_3C6D);
    state ^= state >> 12;

    #[allow(clippy::cast_precision_loss)]
    let unit = (state % 1024) as f32 / 1023.0;
    unit
}

pub(crate) fn upload(gpu: &Gpu, image: &Image) -> wgpu::Texture {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rusplay-test-source"),
        size: extent(image.width, image.height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let bytes: Vec<u8> = image
        .pixels
        .iter()
        .flatten()
        .flat_map(|value| f16::from_f32(*value).to_le_bytes())
        .collect();
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(image.width * RGBA16F_BYTES),
            rows_per_image: None,
        },
        extent(image.width, image.height),
    );

    texture
}

pub(crate) fn storage_target(gpu: &Gpu, width: u32, height: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rusplay-test-target"),
        size: extent(width, height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

pub(crate) fn download(gpu: &Gpu, texture: &wgpu::Texture) -> Image {
    let (width, height) = (texture.width(), texture.height());
    let row_bytes = (width * RGBA16F_BYTES).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let staging = readback_buffer(gpu, u64::from(row_bytes * height));

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: None,
            },
        },
        extent(width, height),
    );
    gpu.queue.submit([encoder.finish()]);

    let bytes = map_read(gpu, &staging);
    let pixels = bytes
        .chunks_exact(row_bytes as usize)
        .flat_map(|row| row[..(width * RGBA16F_BYTES) as usize].as_chunks::<8>().0)
        .map(|texel| {
            std::array::from_fn(|channel| {
                f16::from_le_bytes([texel[2 * channel], texel[2 * channel + 1]]).to_f32()
            })
        })
        .collect();

    Image {
        width,
        height,
        pixels,
    }
}

pub(crate) fn submit(gpu: &Gpu, record: impl FnOnce(&mut wgpu::CommandEncoder)) {
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    record(&mut encoder);
    gpu.queue.submit([encoder.finish()]);
}

fn readback_buffer(gpu: &Gpu, size: u64) -> wgpu::Buffer {
    gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rusplay-test-readback"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn map_read(gpu: &Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
    buffer.map_async(wgpu::MapMode::Read, .., |result| {
        result.expect("readback buffer maps");
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device finishes the submitted work");

    let bytes = buffer
        .get_mapped_range(..)
        .expect("mapped range is readable")
        .to_vec();
    buffer.unmap();
    bytes
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}
