use std::sync::OnceLock;

use half::f16;

use crate::Gpu;

const RGBA16F_BYTES: u32 = 8;

pub(crate) fn gpu() -> Option<Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();

    GPU.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..wgpu::RequestAdapterOptions::default()
        }))
        .ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&rusplay_scale::device_descriptor(&adapter)))
                .ok()?;

        Some(Gpu { device, queue })
    })
    .clone()
}

pub(crate) fn quantized(value: f32) -> f32 {
    f16::from_f32(value).to_f32()
}

pub(crate) fn texture(
    gpu: &Gpu,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rusplay-test-texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: usage | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

pub(crate) fn storage(gpu: &Gpu, width: u32, height: u32) -> wgpu::Texture {
    texture(
        gpu,
        width,
        height,
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
    )
}

pub(crate) fn upload(gpu: &Gpu, width: u32, height: u32, texels: &[[f32; 4]]) -> wgpu::Texture {
    let uploaded = texture(
        gpu,
        width,
        height,
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let bytes: Vec<u8> = texels
        .iter()
        .flatten()
        .flat_map(|channel| f16::from_f32(*channel).to_le_bytes())
        .collect();

    gpu.queue.write_texture(
        uploaded.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * RGBA16F_BYTES),
            rows_per_image: Some(height),
        },
        uploaded.size(),
    );
    uploaded
}

pub(crate) fn submit(gpu: &Gpu, record: impl FnOnce(&mut wgpu::CommandEncoder)) {
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    record(&mut encoder);
    gpu.queue.submit([encoder.finish()]);
}

pub(crate) fn read_bytes(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u8> {
    let (width, height) = (texture.width(), texture.height());
    let texel = texture.format().block_copy_size(None).unwrap_or(4);
    let tight_row = (width * texel) as usize;
    let row_bytes = (width * texel).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rusplay-test-readback"),
        size: u64::from(row_bytes * height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    submit(gpu, |encoder| {
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
            texture.size(),
        );
    });
    staging.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();

    let bytes = staging
        .get_mapped_range(..)
        .unwrap()
        .chunks_exact(row_bytes as usize)
        .flat_map(|row| &row[..tight_row])
        .copied()
        .collect();
    staging.unmap();
    bytes
}

pub(crate) fn read_rgba16f(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    read_bytes(gpu, texture)
        .as_chunks::<8>()
        .0
        .iter()
        .map(|texel| {
            let channel = |index: usize| {
                f16::from_le_bytes([texel[2 * index], texel[2 * index + 1]]).to_f32()
            };
            [channel(0), channel(1), channel(2), channel(3)]
        })
        .collect()
}

pub(crate) fn read_rgb10a2(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<[u32; 3]> {
    read_bytes(gpu, texture)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| {
            let word = u32::from_le_bytes(*word);
            [word & 0x3ff, (word >> 10) & 0x3ff, (word >> 20) & 0x3ff]
        })
        .collect()
}
