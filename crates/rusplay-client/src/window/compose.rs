use std::{borrow::Cow, num::NonZeroU64};

const SHADER: &str = include_str!("compose.wgsl");
const PARAMS_BYTES: u64 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CursorPlacement {
    pub origin: [i32; 2],
    pub size: [u32; 2],
}

#[derive(Debug)]
pub(crate) struct Composer {
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    params: wgpu::Buffer,
    no_cursor: wgpu::TextureView,
}

impl Composer {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rusplay-compose"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(PARAMS_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rusplay-compose"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rusplay-compose"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rusplay-compose-params"),
            size: PARAMS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let no_cursor = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("rusplay-compose-no-cursor"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            layout,
            pipeline_layout,
            shader,
            pipelines: Vec::new(),
            params,
            no_cursor,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        picture: &wgpu::TextureView,
        cursor: Option<(&wgpu::TextureView, CursorPlacement)>,
        target: &wgpu::TextureView,
        format: wgpu::TextureFormat,
    ) {
        let placement = cursor.map_or(
            CursorPlacement {
                origin: [0, 0],
                size: [0, 0],
            },
            |(_, placement)| placement,
        );
        queue.write_buffer(&self.params, 0, &params_bytes(placement));

        let cursor_view = cursor.map_or(&self.no_cursor, |(view, _)| view);
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rusplay-compose"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(picture),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(cursor_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let pipeline = self.pipeline(device, format);

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rusplay-compose"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &wgpu::RenderPipeline {
        let existing = self
            .pipelines
            .iter()
            .position(|(existing, _)| *existing == format);
        let index = existing.unwrap_or_else(|| {
            let pipeline = self.create_pipeline(device, format);
            self.pipelines.push((format, pipeline));
            self.pipelines.len() - 1
        });

        &self.pipelines[index].1
    }

    fn create_pipeline(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rusplay-compose"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("cover"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("compose"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn params_bytes(placement: CursorPlacement) -> [u8; 16] {
    let size = placement
        .size
        .map(|dimension| i32::try_from(dimension).unwrap_or(i32::MAX));
    let words = [placement.origin[0], placement.origin[1], size[0], size[1]];

    let mut bytes = [0; 16];
    for (chunk, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use half::f16;

    use super::*;
    use crate::Gpu;

    const WIDTH: u32 = 8;
    const HEIGHT: u32 = 4;
    const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

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

    fn filled(gpu: &Gpu, width: u32, height: u32, texel: [f32; 4]) -> wgpu::TextureView {
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rusplay-test-input"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes: Vec<u8> = (0..width * height)
            .flat_map(|_| texel)
            .flat_map(|channel| f16::from_f32(channel).to_le_bytes())
            .collect();
        gpu.queue.write_texture(
            texture.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 8),
                rows_per_image: Some(height),
            },
            texture.size(),
        );

        texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn compose(gpu: &Gpu, cursor: Option<([f32; 4], CursorPlacement)>) -> Vec<u8> {
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rusplay-test-target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let picture = filled(gpu, WIDTH, HEIGHT, [0.25, 0.25, 0.25, 1.0]);
        let cursor = cursor.map(|(texel, placement)| {
            (
                filled(gpu, placement.size[0], placement.size[1], texel),
                placement,
            )
        });
        let mut composer = Composer::new(&gpu.device);

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        composer.encode(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &picture,
            cursor.as_ref().map(|(view, placement)| (view, *placement)),
            &target.create_view(&wgpu::TextureViewDescriptor::default()),
            TARGET_FORMAT,
        );

        let row_bytes = (WIDTH * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rusplay-test-readback"),
            size: u64::from(row_bytes * HEIGHT),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: None,
                },
            },
            target.size(),
        );
        gpu.queue.submit([encoder.finish()]);
        staging.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let bytes = staging
            .get_mapped_range(..)
            .unwrap()
            .chunks_exact(row_bytes as usize)
            .flat_map(|row| &row[..(WIDTH * 4) as usize])
            .copied()
            .collect();
        staging.unmap();
        bytes
    }

    fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
        let start = ((y * WIDTH + x) * 4) as usize;
        bytes[start..start + 4].try_into().unwrap()
    }

    fn close(found: [u8; 4], expected: [u8; 4]) -> bool {
        found
            .iter()
            .zip(expected)
            .all(|(found, expected)| found.abs_diff(expected) <= 1)
    }

    #[test]
    fn the_picture_is_copied_one_to_one_without_a_cursor() {
        let Some(gpu) = gpu() else {
            return;
        };

        let bytes = compose(&gpu, None);

        for (x, y) in [(0, 0), (7, 3), (3, 1)] {
            assert!(close(pixel(&bytes, x, y), [64, 64, 64, 255]));
        }
    }

    #[test]
    fn the_cursor_is_blended_premultiplied_inside_its_rectangle_only() {
        let Some(gpu) = gpu() else {
            return;
        };
        let placement = CursorPlacement {
            origin: [3, 1],
            size: [2, 2],
        };

        let bytes = compose(&gpu, Some(([0.5, 0.0, 0.0, 0.5], placement)));

        assert!(close(pixel(&bytes, 3, 1), [159, 32, 32, 255]));
        assert!(close(pixel(&bytes, 4, 2), [159, 32, 32, 255]));
        assert!(close(pixel(&bytes, 2, 1), [64, 64, 64, 255]));
        assert!(close(pixel(&bytes, 5, 1), [64, 64, 64, 255]));
        assert!(close(pixel(&bytes, 3, 3), [64, 64, 64, 255]));
    }

    #[test]
    fn a_cursor_partly_off_the_picture_is_clipped() {
        let Some(gpu) = gpu() else {
            return;
        };
        let placement = CursorPlacement {
            origin: [-1, -1],
            size: [2, 2],
        };

        let bytes = compose(&gpu, Some(([1.0, 1.0, 1.0, 1.0], placement)));

        assert!(close(pixel(&bytes, 0, 0), [255, 255, 255, 255]));
        assert!(close(pixel(&bytes, 1, 0), [64, 64, 64, 255]));
        assert!(close(pixel(&bytes, 0, 1), [64, 64, 64, 255]));
    }
}
