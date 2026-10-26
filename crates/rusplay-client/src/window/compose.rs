use std::{borrow::Cow, num::NonZeroU64};

use rusplay_core::{ColorPrimaries, Matrix3, WhiteAdaptation};

use crate::window::{
    dither::{BAYER_BITS, BLUE_NOISE_SIZE, blue_noise_thresholds},
    output::{DisplayEncoding, DitherPattern, Signal},
};

const SHADER: &str = include_str!("compose.wgsl");
const PARAMS_WORDS: usize = 40;
const PARAMS_BYTES: u64 = PARAMS_WORDS as u64 * 4;
const PQ_PEAK_NITS: f64 = 10_000.0;
const HLG_REFERENCE_GAMMA: f64 = 1.2;
const HLG_GAMMA_PER_DECADE: f64 = 0.42;
const HLG_REFERENCE_PEAK_NITS: f64 = 1000.0;
const NOISE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

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
    encoding: [u32; PARAMS_WORDS],
    params: wgpu::Buffer,
    no_cursor: wgpu::TextureView,
    noise: wgpu::TextureView,
}

impl Composer {
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoding: &DisplayEncoding,
    ) -> Self {
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
                texture_entry(3),
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
        let no_cursor = single_texture(
            device,
            "rusplay-compose-no-cursor",
            1,
            wgpu::TextureFormat::Rgba16Float,
        )
        .create_view(&wgpu::TextureViewDescriptor::default());
        let noise = upload_noise(device, queue);

        Self {
            layout,
            pipeline_layout,
            shader,
            pipelines: Vec::new(),
            encoding: encoding_words(encoding),
            params,
            no_cursor,
            noise,
        }
    }

    pub(crate) fn set_encoding(&mut self, encoding: &DisplayEncoding) {
        self.encoding = encoding_words(encoding);
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
        queue.write_buffer(&self.params, 0, &params_bytes(placement, &self.encoding));

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
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.noise),
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

fn single_texture(
    device: &wgpu::Device,
    label: &str,
    size: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn upload_noise(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let size = u32::try_from(BLUE_NOISE_SIZE).unwrap_or(u32::MAX);
    let texture = single_texture(device, "rusplay-compose-blue-noise", size, NOISE_FORMAT);
    let bytes: Vec<u8> = blue_noise_thresholds()
        .iter()
        .flat_map(|threshold| threshold.to_le_bytes())
        .collect();

    queue.write_texture(
        texture.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * 4),
            rows_per_image: Some(size),
        },
        texture.size(),
    );

    texture.create_view(&wgpu::TextureViewDescriptor::default())
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

const fn transfer_code(signal: Signal) -> u32 {
    match signal {
        Signal::Linear => 0,
        Signal::Srgb => 1,
        Signal::Bt709 => 2,
        Signal::Gamma22 => 3,
        Signal::Pq { .. } => 4,
        Signal::Hlg { .. } => 5,
        Signal::ExtendedLinear => 6,
    }
}

pub(crate) fn hlg_exponent(peak_nits: f64) -> f64 {
    let gamma =
        HLG_REFERENCE_GAMMA + HLG_GAMMA_PER_DECADE * (peak_nits / HLG_REFERENCE_PEAK_NITS).log10();

    (1.0 - gamma) / gamma
}

pub(crate) fn signal_scale(signal: Signal) -> f64 {
    match signal {
        Signal::Pq { white_nits } => white_nits / PQ_PEAK_NITS,
        Signal::Hlg {
            white_nits,
            peak_nits,
        } => white_nits / peak_nits,
        _ => 1.0,
    }
}

fn float(value: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let narrowed = value as f32;
    narrowed.to_bits()
}

fn push_rows(words: &mut Vec<u32>, matrix: Matrix3) {
    for row in matrix.rows() {
        words.extend(row.map(float));
        words.push(0);
    }
}

fn encoding_words(encoding: &DisplayEncoding) -> [u32; PARAMS_WORDS] {
    let target = encoding.target.primaries;
    let mut words = Vec::with_capacity(PARAMS_WORDS);
    words.extend([0; 4]);
    push_rows(
        &mut words,
        ColorPrimaries::Bt709.rgb_to_rgb(target, WhiteAdaptation::Bradford),
    );
    push_rows(
        &mut words,
        target.rgb_to_rgb(encoding.primaries, WhiteAdaptation::Bradford),
    );
    words.extend(encoding.primaries.rgb_to_xyz().rows()[1].map(float));
    words.push(0);

    let hlg_peak = match encoding.signal {
        Signal::Hlg { peak_nits, .. } => peak_nits,
        _ => HLG_REFERENCE_PEAK_NITS,
    };
    let steps = encoding
        .dither
        .map_or(1, |dither| (1u32 << dither.bits) - 1);
    words.extend([
        float(signal_scale(encoding.signal)),
        float(hlg_exponent(hlg_peak)),
        float(f64::from(steps)),
        0,
    ]);
    words.extend([
        transfer_code(encoding.signal),
        match encoding.dither.map(|dither| dither.pattern) {
            None => 0,
            Some(DitherPattern::Ordered) => 1,
            Some(DitherPattern::BlueNoise) => 2,
        },
        u32::try_from(BLUE_NOISE_SIZE).unwrap_or(u32::MAX),
        BAYER_BITS,
    ]);

    let mut fixed = [0; PARAMS_WORDS];
    fixed.copy_from_slice(&words);
    fixed
}

fn params_bytes(placement: CursorPlacement, encoding: &[u32; PARAMS_WORDS]) -> Vec<u8> {
    let size = placement
        .size
        .map(|dimension| i32::try_from(dimension).unwrap_or(i32::MAX));
    let cursor = [placement.origin[0], placement.origin[1], size[0], size[1]];

    cursor
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .chain(
            encoding[cursor.len()..]
                .iter()
                .flat_map(|word| word.to_le_bytes()),
        )
        .collect()
}

#[cfg(test)]
mod tests;
