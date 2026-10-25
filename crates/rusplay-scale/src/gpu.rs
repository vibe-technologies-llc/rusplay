use std::borrow::Cow;

use wgpu::util::DeviceExt;

const RESAMPLE_SHADER: &str = concat!(include_str!("shaping.wgsl"), include_str!("resample.wgsl"));
const CONVERT_LUMA_SHADER: &str = concat!(
    include_str!("convert_common.wgsl"),
    include_str!("convert_luma.wgsl")
);
const CONVERT_CHROMA_SHADER: &str = concat!(
    include_str!("convert_common.wgsl"),
    include_str!("convert_chroma.wgsl")
);
const PACK_CHROMA_SHADER: &str = concat!(
    include_str!("convert_common.wgsl"),
    include_str!("convert_pack.wgsl")
);
const POLAR_SHADER: &str = concat!(include_str!("shaping.wgsl"), include_str!("polar.wgsl"));

pub(crate) const WORKGROUP_SIDE: u32 = 8;
pub(crate) const INTERMEDIATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub(crate) const CHROMA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;

#[derive(Debug, Clone)]
pub struct ScalePipelines {
    pub(crate) source_layout: wgpu::BindGroupLayout,
    pub(crate) destination_layout: wgpu::BindGroupLayout,
    pub(crate) separable_layout: wgpu::BindGroupLayout,
    pub(crate) separable: wgpu::ComputePipeline,
    pub(crate) polar_layout: wgpu::BindGroupLayout,
    pub(crate) polar: wgpu::ComputePipeline,
    pub(crate) output_layout: wgpu::BindGroupLayout,
    pub(crate) chroma_target_layout: wgpu::BindGroupLayout,
    pub(crate) convert_params_layout: wgpu::BindGroupLayout,
    pub(crate) convert_table_layout: wgpu::BindGroupLayout,
    pub(crate) convert_luma: wgpu::ComputePipeline,
    pub(crate) convert_chroma: wgpu::ComputePipeline,
    pub(crate) pack_chroma: wgpu::ComputePipeline,
}

impl ScalePipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let source_layout = layout(device, "rusplay-scale-source", &[texture_entry(0)]);
        let destination_layout = layout(
            device,
            "rusplay-scale-destination",
            &[storage_texture_entry(0, INTERMEDIATE_FORMAT)],
        );
        let separable_layout = layout(
            device,
            "rusplay-scale-separable",
            &[
                storage_buffer_entry(0),
                storage_buffer_entry(1),
                storage_buffer_entry(2),
                uniform_entry(3),
            ],
        );

        let separable = compute_pipeline(
            device,
            "rusplay-resample",
            &[&source_layout, &destination_layout, &separable_layout],
            RESAMPLE_SHADER,
            "resample",
        );

        let polar_layout = layout(
            device,
            "rusplay-scale-polar",
            &[storage_buffer_entry(0), uniform_entry(1)],
        );
        let polar = compute_pipeline(
            device,
            "rusplay-resample-polar",
            &[&source_layout, &destination_layout, &polar_layout],
            POLAR_SHADER,
            "resample_polar",
        );

        let output_layout = layout(
            device,
            "rusplay-convert-output",
            &[writable_storage_buffer_entry(0)],
        );
        let chroma_target_layout = layout(
            device,
            "rusplay-convert-chroma-target",
            &[storage_texture_entry(0, CHROMA_FORMAT)],
        );
        let convert_params_layout = layout(device, "rusplay-convert-params", &[uniform_entry(0)]);
        let convert_table_layout = layout(
            device,
            "rusplay-convert-table",
            &[
                uniform_entry(0),
                storage_buffer_entry(1),
                storage_buffer_entry(2),
            ],
        );
        let convert_luma = compute_pipeline(
            device,
            "rusplay-convert-luma",
            &[&source_layout, &output_layout, &convert_params_layout],
            CONVERT_LUMA_SHADER,
            "convert_luma",
        );
        let convert_chroma = compute_pipeline(
            device,
            "rusplay-convert-chroma",
            &[&source_layout, &chroma_target_layout, &convert_table_layout],
            CONVERT_CHROMA_SHADER,
            "convert_chroma",
        );
        let pack_chroma = compute_pipeline(
            device,
            "rusplay-pack-chroma",
            &[&source_layout, &output_layout, &convert_table_layout],
            PACK_CHROMA_SHADER,
            "pack_chroma",
        );

        Self {
            source_layout,
            destination_layout,
            separable_layout,
            separable,
            polar_layout,
            polar,
            output_layout,
            chroma_target_layout,
            convert_params_layout,
            convert_table_layout,
            convert_luma,
            convert_chroma,
            pack_chroma,
        }
    }

    pub(crate) fn source_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        view_group(device, &self.source_layout, view)
    }

    pub(crate) fn destination_group(
        &self,
        device: &wgpu::Device,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        view_group(device, &self.destination_layout, view)
    }
}

pub(crate) fn layout(
    device: &wgpu::Device,
    label: &str,
    entries: &[wgpu::BindGroupLayoutEntry],
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries,
    })
}

pub(crate) fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    layouts: &[&wgpu::BindGroupLayout],
    shader: &str,
    entry_point: &str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(shader)),
    });
    let bind_group_layouts: Vec<Option<&wgpu::BindGroupLayout>> =
        layouts.iter().copied().map(Some).collect();
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &bind_group_layouts,
        immediate_size: 0,
    });

    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some(entry_point),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}

pub(crate) fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    compute_entry(
        binding,
        wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
    )
}

pub(crate) fn storage_texture_entry(
    binding: u32,
    format: wgpu::TextureFormat,
) -> wgpu::BindGroupLayoutEntry {
    compute_entry(
        binding,
        wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
    )
}

pub(crate) fn storage_buffer_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    buffer_entry(
        binding,
        wgpu::BufferBindingType::Storage { read_only: true },
    )
}

pub(crate) fn writable_storage_buffer_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    buffer_entry(
        binding,
        wgpu::BufferBindingType::Storage { read_only: false },
    )
}

pub(crate) fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    buffer_entry(binding, wgpu::BufferBindingType::Uniform)
}

fn buffer_entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    compute_entry(
        binding,
        wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
    )
}

fn compute_entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}

pub(crate) fn view_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rusplay-scale-view"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(view),
        }],
    })
}

pub(crate) fn buffer_group(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    buffers: &[&wgpu::Buffer],
) -> wgpu::BindGroup {
    let entries: Vec<wgpu::BindGroupEntry<'_>> = (0..)
        .zip(buffers)
        .map(|(binding, buffer)| wgpu::BindGroupEntry {
            binding,
            resource: buffer.as_entire_binding(),
        })
        .collect();

    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &entries,
    })
}

pub(crate) trait NativeBytes: Copy {
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

pub(crate) fn to_bytes<T: NativeBytes>(values: &[T]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.native_bytes())
        .collect()
}

pub(crate) fn storage_buffer<T: NativeBytes>(
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

pub(crate) fn uniform_buffer(device: &wgpu::Device, label: &str, contents: &[u8]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

pub(crate) fn storage_texture(
    device: &wgpu::Device,
    label: &str,
    size: (u32, u32),
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

pub(crate) fn dispatch(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    pipeline: &wgpu::ComputePipeline,
    groups: &[&wgpu::BindGroup],
    size: (u32, u32),
) {
    dispatch_layers(encoder, label, pipeline, groups, size, 1);
}

pub(crate) fn dispatch_layers(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    pipeline: &wgpu::ComputePipeline,
    groups: &[&wgpu::BindGroup],
    size: (u32, u32),
    layers: u32,
) {
    let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    });
    compute.set_pipeline(pipeline);
    for (index, group) in (0..).zip(groups) {
        compute.set_bind_group(index, *group, &[]);
    }
    compute.dispatch_workgroups(
        size.0.div_ceil(WORKGROUP_SIDE),
        size.1.div_ceil(WORKGROUP_SIDE),
        layers,
    );
}
