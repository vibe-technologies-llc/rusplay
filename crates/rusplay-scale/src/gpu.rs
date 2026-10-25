use std::borrow::Cow;

use wgpu::util::DeviceExt;

use crate::TapTable;

const SHADER: &str = include_str!("resample.wgsl");
const WORKGROUP_SIDE: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    const fn index(self) -> u32 {
        match self {
            Self::Horizontal => 0,
            Self::Vertical => 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScalePass<'a> {
    pub axis: Axis,
    pub table: &'a TapTable,
    pub source: &'a wgpu::TextureView,
    pub source_size: (u32, u32),
    pub target: &'a wgpu::TextureView,
    pub target_size: (u32, u32),
}

#[derive(Debug)]
pub struct GpuScaler {
    pipeline: wgpu::ComputePipeline,
}

impl GpuScaler {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rusplay-resample"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rusplay-resample"),
            layout: None,
            module: &module,
            entry_point: Some("resample"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        Self { pipeline }
    }

    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pass: &ScalePass<'_>,
    ) {
        let firsts = storage_buffer(device, "rusplay-resample-firsts", &pass.table.firsts);
        let weights = storage_buffer(device, "rusplay-resample-weights", &pass.table.weights);
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rusplay-resample-params"),
            contents: &params_bytes(pass),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rusplay-resample"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(pass.source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(pass.target),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: firsts.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: weights.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: params.as_entire_binding(),
                },
            ],
        });

        let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rusplay-resample"),
            timestamp_writes: None,
        });
        compute.set_pipeline(&self.pipeline);
        compute.set_bind_group(0, &bind_group, &[]);
        compute.dispatch_workgroups(
            pass.target_size.0.div_ceil(WORKGROUP_SIDE),
            pass.target_size.1.div_ceil(WORKGROUP_SIDE),
            1,
        );
    }
}

trait NativeBytes: Copy {
    fn native_bytes(self) -> [u8; 4];
}

impl NativeBytes for i32 {
    fn native_bytes(self) -> [u8; 4] {
        self.to_ne_bytes()
    }
}

impl NativeBytes for f32 {
    fn native_bytes(self) -> [u8; 4] {
        self.to_ne_bytes()
    }
}

impl NativeBytes for u32 {
    fn native_bytes(self) -> [u8; 4] {
        self.to_ne_bytes()
    }
}

fn to_bytes<T: NativeBytes>(values: &[T]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.native_bytes())
        .collect()
}

fn storage_buffer<T: NativeBytes>(
    device: &wgpu::Device,
    label: &str,
    values: &[T],
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: &to_bytes(values),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

fn params_bytes(pass: &ScalePass<'_>) -> Vec<u8> {
    to_bytes(&[
        pass.source_size.0,
        pass.source_size.1,
        pass.target_size.0,
        pass.target_size.1,
        pass.table.taps_per_pixel,
        pass.axis.index(),
        0,
        0,
    ])
}

#[cfg(test)]
mod tests {
    use rusplay_core::ScaleFilter;

    use super::*;
    use crate::{
        Kernel,
        reference::resample_separable,
        testing::{self, Image},
    };

    const RGBA16F_TOLERANCE: f32 = 4.0 / 1024.0;

    fn run_separable(filter: ScaleFilter, source: (u32, u32), target: (u32, u32)) {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let kernel = Kernel::new(filter);
        let horizontal = TapTable::build(kernel, source.0, target.0);
        let vertical = TapTable::build(kernel, source.1, target.1);
        let image = Image::pattern(source.0, source.1, 7);

        let source_texture = testing::upload(gpu, &image);
        let intermediate = testing::storage_target(gpu, target.0, source.1);
        let output = testing::storage_target(gpu, target.0, target.1);
        let source_view = source_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let intermediate_view = intermediate.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

        let scaler = GpuScaler::new(&gpu.device);
        testing::submit(gpu, |encoder| {
            scaler.encode(
                &gpu.device,
                encoder,
                &ScalePass {
                    axis: Axis::Horizontal,
                    table: &horizontal,
                    source: &source_view,
                    source_size: source,
                    target: &intermediate_view,
                    target_size: (target.0, source.1),
                },
            );
            scaler.encode(
                &gpu.device,
                encoder,
                &ScalePass {
                    axis: Axis::Vertical,
                    table: &vertical,
                    source: &intermediate_view,
                    source_size: (target.0, source.1),
                    target: &output_view,
                    target_size: target,
                },
            );
        });

        let actual = testing::download(gpu, &output);
        let expected = resample_separable(
            &resample_separable(&image, &horizontal, Axis::Horizontal),
            &vertical,
            Axis::Vertical,
        );

        let difference = actual.max_difference(&expected);
        assert!(
            difference <= RGBA16F_TOLERANCE,
            "{filter} {source:?} -> {target:?}: max difference {difference}"
        );
    }

    #[test]
    fn hermite_downscale_matches_reference() {
        run_separable(ScaleFilter::Hermite, (97, 61), (40, 27));
    }

    #[test]
    fn lanczos3_upscale_matches_reference() {
        run_separable(ScaleFilter::Lanczos3, (37, 29), (91, 70));
    }

    #[test]
    fn every_separable_filter_matches_reference_both_ways() {
        for filter in [
            ScaleFilter::Bilinear,
            ScaleFilter::Hermite,
            ScaleFilter::CatmullRom,
            ScaleFilter::Mitchell,
            ScaleFilter::Lanczos2,
            ScaleFilter::Lanczos3,
            ScaleFilter::Lanczos4,
        ] {
            run_separable(filter, (64, 48), (23, 17));
            run_separable(filter, (23, 17), (64, 48));
        }
    }

    #[test]
    fn identity_scale_reproduces_the_source() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let kernel = Kernel::new(ScaleFilter::Lanczos3);
        let table = TapTable::build(kernel, 33, 33);
        let image = Image::pattern(33, 21, 3);

        let source = testing::upload(gpu, &image);
        let output = testing::storage_target(gpu, 33, 21);
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

        let scaler = GpuScaler::new(&gpu.device);
        testing::submit(gpu, |encoder| {
            scaler.encode(
                &gpu.device,
                encoder,
                &ScalePass {
                    axis: Axis::Horizontal,
                    table: &table,
                    source: &source_view,
                    source_size: (33, 21),
                    target: &output_view,
                    target_size: (33, 21),
                },
            );
        });

        assert!(testing::download(gpu, &output).max_difference(&image) <= 1.0 / 1024.0);
    }
}
