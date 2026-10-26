use std::borrow::Cow;

use wgpu::util::DeviceExt;

use crate::color::{
    BOUNDARY_APPROACH_STEPS, BOUNDARY_COARSE_STEPS, BOUNDARY_HEADROOM, BOUNDARY_LOWER_TOLERANCE,
    BOUNDARY_RAMP_CENTRE, BOUNDARY_REACH, BOUNDARY_SUBSTEPS, BOUNDARY_UPPER_TOLERANCE,
    BOUNDARY_WINDOW_START, BOUNDARY_WINDOW_STEPS, COMPRESSION_POWER, HISTOGRAM_BINS,
};

const RESAMPLE_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("shaping.wgsl"),
    include_str!("resample.wgsl")
);
const CONVERT_DIRECT_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("convert_common.wgsl"),
    include_str!("convert_direct.wgsl")
);
const CONVERT_ROWS_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("convert_common.wgsl"),
    include_str!("convert_rows.wgsl")
);
const PACK_CHROMA_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("convert_common.wgsl"),
    include_str!("convert_pack.wgsl")
);
const UNPACK_CHROMA_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("unpack_common.wgsl"),
    include_str!("unpack_buffer.wgsl"),
    include_str!("unpack_chroma.wgsl")
);
const UNPACK_RGB_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("unpack_common.wgsl"),
    include_str!("unpack_buffer.wgsl"),
    include_str!("unpack_output_transfer.wgsl"),
    include_str!("unpack_rgb.wgsl")
);
const UNPACK_RGB_LUT_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("unpack_common.wgsl"),
    include_str!("unpack_buffer.wgsl"),
    include_str!("transfer_decode.wgsl"),
    include_str!("color_lut_storage.wgsl"),
    include_str!("color_lut_sample.wgsl"),
    include_str!("unpack_output_lut.wgsl"),
    include_str!("unpack_rgb.wgsl")
);
const UNPACK_PLANES_CHROMA_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("unpack_common.wgsl"),
    include_str!("unpack_planes.wgsl"),
    include_str!("unpack_chroma.wgsl")
);
const UNPACK_PLANES_RGB_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("unpack_common.wgsl"),
    include_str!("unpack_planes.wgsl"),
    include_str!("unpack_output_transfer.wgsl"),
    include_str!("unpack_rgb.wgsl")
);
const UNPACK_PLANES_RGB_LUT_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("unpack_common.wgsl"),
    include_str!("unpack_planes.wgsl"),
    include_str!("transfer_decode.wgsl"),
    include_str!("color_lut_storage.wgsl"),
    include_str!("color_lut_sample.wgsl"),
    include_str!("unpack_output_lut.wgsl"),
    include_str!("unpack_rgb.wgsl")
);
const POLAR_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("shaping.wgsl"),
    include_str!("destination_rgba16float.wgsl"),
    include_str!("polar.wgsl")
);
const POLAR_CHROMA_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("shaping.wgsl"),
    include_str!("destination_rg32float.wgsl"),
    include_str!("polar.wgsl")
);
const CONVERT_FULL_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("convert_common.wgsl"),
    include_str!("convert_full.wgsl")
);

const COLOR_MAP_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("color_common.wgsl"),
    include_str!("color_tone.wgsl"),
    include_str!("color_gamut.wgsl"),
    include_str!("color_light.wgsl"),
    include_str!("color_map.wgsl")
);
const COLOR_LUT_BAKE_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("color_common.wgsl"),
    include_str!("color_tone.wgsl"),
    include_str!("color_gamut.wgsl"),
    include_str!("color_light.wgsl"),
    include_str!("transfer_decode.wgsl"),
    include_str!("color_lut_storage.wgsl"),
    include_str!("color_lut_bake.wgsl")
);

const COLOR_HISTOGRAM_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("color_common.wgsl"),
    include_str!("color_tone.wgsl"),
    include_str!("color_histogram.wgsl")
);
const COLOR_PEAK_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("color_common.wgsl"),
    include_str!("color_tone.wgsl"),
    include_str!("color_peak.wgsl")
);

const LINEARISE_FLOAT_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("destination_rgba16float.wgsl"),
    include_str!("linearise_float.wgsl"),
    include_str!("transfer_decode.wgsl"),
    include_str!("linearise_common.wgsl")
);
const LINEARISE_PACKED_SHADER: &str = concat!(
    include_str!("workgroup.wgsl"),
    include_str!("destination_rgba16float.wgsl"),
    include_str!("linearise_packed.wgsl"),
    include_str!("transfer_decode.wgsl"),
    include_str!("linearise_common.wgsl")
);

const SEPARABLE_WORKGROUP: (u32, u32) = (8, 8);
const POLAR_WORKGROUP: (u32, u32) = (64, 1);
const CONVERT_WORKGROUP: (u32, u32) = (8, 8);
const CONVERT_ROWS_WORKGROUP: (u32, u32) = (64, 1);
const COLOR_WORKGROUP: (u32, u32) = (8, 8);
const LUT_BAKE_WORKGROUP: (u32, u32) = (8, 8);
const LINEARISE_WORKGROUP: (u32, u32) = (8, 8);
const HISTOGRAM_WORKGROUP: (u32, u32) = (16, 16);
const PEAK_WORKGROUP: (u32, u32) = (1, 1);

const _: () = assert!(HISTOGRAM_WORKGROUP.0 * HISTOGRAM_WORKGROUP.1 == HISTOGRAM_BINS);

pub(crate) const MAX_CHROMA_TAPS: u32 = 32;

pub(crate) const INTERMEDIATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub(crate) const CHROMA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;
pub(crate) const LUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Debug, Clone)]
pub(crate) struct ShaderPass {
    pub pipeline: wgpu::ComputePipeline,
    pub workgroup: (u32, u32),
}

#[derive(Debug, Clone)]
pub struct ScalePipelines {
    pub(crate) source_layout: wgpu::BindGroupLayout,
    pub(crate) destination_layout: wgpu::BindGroupLayout,
    pub(crate) separable_layout: wgpu::BindGroupLayout,
    pub(crate) separable: ShaderPass,
    pub(crate) polar_layout: wgpu::BindGroupLayout,
    pub(crate) polar: ShaderPass,
    pub(crate) convert: ConvertPipelines,
    pub(crate) unpack: UnpackPasses,
    pub(crate) color: ColorPipelines,
    pub(crate) linearise: LinearisePipelines,
}

#[derive(Debug, Clone)]
pub(crate) struct LinearisePipelines {
    pub params_layout: wgpu::BindGroupLayout,
    pub packed_source_layout: wgpu::BindGroupLayout,
    pub float: ShaderPass,
    pub packed: ShaderPass,
}

impl LinearisePipelines {
    fn new(
        device: &wgpu::Device,
        source_layout: &wgpu::BindGroupLayout,
        destination_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let params_layout = layout(device, "rusplay-linearise-params", &[uniform_entry(0)]);
        let packed_source_layout = layout(
            device,
            "rusplay-linearise-packed-source",
            &[uint_texture_entry(0)],
        );
        let float = compute_pipeline(
            device,
            "rusplay-linearise",
            &[source_layout, destination_layout, &params_layout],
            LINEARISE_FLOAT_SHADER,
            "linearise",
            LINEARISE_WORKGROUP,
            &[],
        );
        let packed = compute_pipeline(
            device,
            "rusplay-linearise-packed",
            &[&packed_source_layout, destination_layout, &params_layout],
            LINEARISE_PACKED_SHADER,
            "linearise",
            LINEARISE_WORKGROUP,
            &[],
        );

        Self {
            params_layout,
            packed_source_layout,
            float,
            packed,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UnpackPasses {
    pub input_layout: wgpu::BindGroupLayout,
    pub chroma: ShaderPass,
    pub rgb: ShaderPass,
    pub planes_layout: wgpu::BindGroupLayout,
    pub planes_chroma: ShaderPass,
    pub planes_rgb: ShaderPass,
    pub lut_target_layout: wgpu::BindGroupLayout,
    pub rgb_lut: ShaderPass,
    pub planes_rgb_lut: ShaderPass,
}

#[derive(Debug, Clone)]
pub(crate) struct ColorPipelines {
    pub params_layout: wgpu::BindGroupLayout,
    pub analysis_layout: wgpu::BindGroupLayout,
    pub map: ShaderPass,
    pub histogram: ShaderPass,
    pub peak: ShaderPass,
    pub lut_store_layout: wgpu::BindGroupLayout,
    pub lut_bake_layout: wgpu::BindGroupLayout,
    pub lut_bake: ShaderPass,
}

impl ColorPipelines {
    fn new(
        device: &wgpu::Device,
        source_layout: &wgpu::BindGroupLayout,
        destination_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let params_layout = layout(
            device,
            "rusplay-color-params",
            &[uniform_entry(0), storage_buffer_entry(1)],
        );
        let analysis_layout = layout(
            device,
            "rusplay-color-analysis",
            &[
                uniform_entry(0),
                writable_storage_buffer_entry(1),
                writable_storage_buffer_entry(2),
            ],
        );
        let gamut_constants = [
            ("BOUNDARY_COARSE_STEPS", f64::from(BOUNDARY_COARSE_STEPS)),
            (
                "BOUNDARY_APPROACH_STEPS",
                f64::from(BOUNDARY_APPROACH_STEPS),
            ),
            ("BOUNDARY_WINDOW_STEPS", f64::from(BOUNDARY_WINDOW_STEPS)),
            ("BOUNDARY_WINDOW_START", BOUNDARY_WINDOW_START),
            ("BOUNDARY_SUBSTEPS", f64::from(BOUNDARY_SUBSTEPS)),
            ("BOUNDARY_LOWER_TOLERANCE", BOUNDARY_LOWER_TOLERANCE),
            ("BOUNDARY_UPPER_TOLERANCE", BOUNDARY_UPPER_TOLERANCE),
            ("BOUNDARY_RAMP_CENTRE", BOUNDARY_RAMP_CENTRE),
            ("BOUNDARY_REACH", BOUNDARY_REACH),
            ("BOUNDARY_HEADROOM", BOUNDARY_HEADROOM),
            ("COMPRESSION_POWER", COMPRESSION_POWER),
        ];
        let map = compute_pipeline(
            device,
            "rusplay-color-map",
            &[source_layout, destination_layout, &params_layout],
            COLOR_MAP_SHADER,
            "map_color",
            COLOR_WORKGROUP,
            &gamut_constants,
        );
        let lut_store_layout = layout(
            device,
            "rusplay-color-lut-store",
            &[storage_texture_3d_entry(0, LUT_FORMAT)],
        );
        let lut_bake_layout = layout(device, "rusplay-color-lut-bake", &[uniform_entry(0)]);
        let lut_bake = compute_pipeline(
            device,
            "rusplay-color-lut-bake",
            &[&lut_store_layout, &params_layout, &lut_bake_layout],
            COLOR_LUT_BAKE_SHADER,
            "bake_lut",
            LUT_BAKE_WORKGROUP,
            &gamut_constants,
        );
        let bins = [("HISTOGRAM_BINS", f64::from(HISTOGRAM_BINS))];
        let histogram = compute_pipeline(
            device,
            "rusplay-color-histogram",
            &[source_layout, &analysis_layout],
            COLOR_HISTOGRAM_SHADER,
            "measure_histogram",
            HISTOGRAM_WORKGROUP,
            &bins,
        );
        let peak = compute_pipeline(
            device,
            "rusplay-color-peak",
            &[&analysis_layout],
            COLOR_PEAK_SHADER,
            "detect_peak",
            PEAK_WORKGROUP,
            &bins,
        );

        Self {
            params_layout,
            analysis_layout,
            map,
            histogram,
            peak,
            lut_store_layout,
            lut_bake_layout,
            lut_bake,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ConvertPipelines {
    pub output_layout: wgpu::BindGroupLayout,
    pub chroma_target_layout: wgpu::BindGroupLayout,
    pub params_layout: wgpu::BindGroupLayout,
    pub table_layout: wgpu::BindGroupLayout,
    pub direct: ShaderPass,
    pub rows: ShaderPass,
    pub full: ShaderPass,
    pub polar_chroma: ShaderPass,
    pub pack_chroma: ShaderPass,
}

impl ConvertPipelines {
    fn new(
        device: &wgpu::Device,
        source_layout: &wgpu::BindGroupLayout,
        polar_layout: &wgpu::BindGroupLayout,
    ) -> Self {
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
        let params_layout = layout(device, "rusplay-convert-params", &[uniform_entry(0)]);
        let table_layout = layout(
            device,
            "rusplay-convert-table",
            &[
                uniform_entry(0),
                storage_buffer_entry(1),
                storage_buffer_entry(2),
            ],
        );
        let direct = compute_pipeline(
            device,
            "rusplay-convert-direct",
            &[source_layout, &output_layout, &params_layout],
            CONVERT_DIRECT_SHADER,
            "convert_direct",
            CONVERT_WORKGROUP,
            &[],
        );
        let rows = compute_pipeline(
            device,
            "rusplay-convert-rows",
            &[
                source_layout,
                &output_layout,
                &table_layout,
                &chroma_target_layout,
            ],
            CONVERT_ROWS_SHADER,
            "convert_rows",
            CONVERT_ROWS_WORKGROUP,
            &[("MAX_CHROMA_TAPS", f64::from(MAX_CHROMA_TAPS))],
        );
        let full = compute_pipeline(
            device,
            "rusplay-convert-full",
            &[
                source_layout,
                &output_layout,
                &params_layout,
                &chroma_target_layout,
            ],
            CONVERT_FULL_SHADER,
            "convert_full",
            CONVERT_WORKGROUP,
            &[],
        );
        let polar_chroma = compute_pipeline(
            device,
            "rusplay-resample-polar-chroma",
            &[source_layout, &chroma_target_layout, polar_layout],
            POLAR_CHROMA_SHADER,
            "resample_polar",
            POLAR_WORKGROUP,
            &[],
        );
        let pack_chroma = compute_pipeline(
            device,
            "rusplay-pack-chroma",
            &[source_layout, &output_layout, &table_layout],
            PACK_CHROMA_SHADER,
            "pack_chroma",
            CONVERT_WORKGROUP,
            &[],
        );

        Self {
            output_layout,
            chroma_target_layout,
            params_layout,
            table_layout,
            direct,
            rows,
            full,
            polar_chroma,
            pack_chroma,
        }
    }
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
            SEPARABLE_WORKGROUP,
            &[],
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
            POLAR_WORKGROUP,
            &[],
        );

        let convert = ConvertPipelines::new(device, &source_layout, &polar_layout);
        let color = ColorPipelines::new(device, &source_layout, &destination_layout);
        let linearise = LinearisePipelines::new(device, &source_layout, &destination_layout);

        let unpack = unpack_passes(
            device,
            [
                &source_layout,
                &destination_layout,
                &convert.chroma_target_layout,
                &convert.table_layout,
            ],
        );

        Self {
            source_layout,
            destination_layout,
            separable_layout,
            separable,
            polar_layout,
            polar,
            convert,
            unpack,
            color,
            linearise,
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

fn unpack_passes(device: &wgpu::Device, layouts: [&wgpu::BindGroupLayout; 4]) -> UnpackPasses {
    let input_layout = layout(device, "rusplay-unpack-input", &[storage_buffer_entry(0)]);
    let planes_layout = layout(
        device,
        "rusplay-unpack-planes",
        &[uint_texture_entry(0), uint_texture_entry(1)],
    );
    let [chroma, rgb] = unpack_pair(
        device,
        "rusplay-unpack",
        &input_layout,
        layouts,
        [UNPACK_CHROMA_SHADER, UNPACK_RGB_SHADER],
    );
    let [planes_chroma, planes_rgb] = unpack_pair(
        device,
        "rusplay-unpack-planes",
        &planes_layout,
        layouts,
        [UNPACK_PLANES_CHROMA_SHADER, UNPACK_PLANES_RGB_SHADER],
    );
    let lut_target_layout = layout(
        device,
        "rusplay-unpack-lut-target",
        &[
            storage_texture_entry(0, INTERMEDIATE_FORMAT),
            texture_3d_entry(1),
            uniform_entry(2),
        ],
    );
    let [source_layout, _, _, convert_table_layout] = layouts;
    let lut_pass = |label: &str, input_layout: &wgpu::BindGroupLayout, shader: &str| {
        compute_pipeline(
            device,
            label,
            &[
                input_layout,
                source_layout,
                &lut_target_layout,
                convert_table_layout,
            ],
            shader,
            "unpack_rgb",
            CONVERT_WORKGROUP,
            &[],
        )
    };
    let rgb_lut = lut_pass(
        "rusplay-unpack-rgb-lut",
        &input_layout,
        UNPACK_RGB_LUT_SHADER,
    );
    let planes_rgb_lut = lut_pass(
        "rusplay-unpack-planes-rgb-lut",
        &planes_layout,
        UNPACK_PLANES_RGB_LUT_SHADER,
    );

    UnpackPasses {
        input_layout,
        chroma,
        rgb,
        planes_layout,
        planes_chroma,
        planes_rgb,
        lut_target_layout,
        rgb_lut,
        planes_rgb_lut,
    }
}

fn unpack_pair(
    device: &wgpu::Device,
    label: &str,
    input_layout: &wgpu::BindGroupLayout,
    layouts: [&wgpu::BindGroupLayout; 4],
    shaders: [&str; 2],
) -> [ShaderPass; 2] {
    let [
        source_layout,
        destination_layout,
        chroma_target_layout,
        convert_table_layout,
    ] = layouts;
    let [chroma_shader, rgb_shader] = shaders;

    let chroma = compute_pipeline(
        device,
        &format!("{label}-chroma"),
        &[input_layout, chroma_target_layout, convert_table_layout],
        chroma_shader,
        "unpack_chroma",
        CONVERT_WORKGROUP,
        &[],
    );
    let rgb = compute_pipeline(
        device,
        &format!("{label}-rgb"),
        &[
            input_layout,
            source_layout,
            destination_layout,
            convert_table_layout,
        ],
        rgb_shader,
        "unpack_rgb",
        CONVERT_WORKGROUP,
        &[],
    );

    [chroma, rgb]
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
    workgroup: (u32, u32),
    overrides: &[(&str, f64)],
) -> ShaderPass {
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

    let constants: Vec<(&str, f64)> = [
        ("WORKGROUP_X", f64::from(workgroup.0)),
        ("WORKGROUP_Y", f64::from(workgroup.1)),
    ]
    .into_iter()
    .chain(overrides.iter().copied())
    .collect();

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some(entry_point),
        compilation_options: wgpu::PipelineCompilationOptions {
            constants: &constants,
            ..wgpu::PipelineCompilationOptions::default()
        },
        cache: None,
    });

    ShaderPass {
        pipeline,
        workgroup,
    }
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

fn texture_3d_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    compute_entry(
        binding,
        wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D3,
            multisampled: false,
        },
    )
}

fn storage_texture_3d_entry(
    binding: u32,
    format: wgpu::TextureFormat,
) -> wgpu::BindGroupLayoutEntry {
    compute_entry(
        binding,
        wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D3,
        },
    )
}

fn uint_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    compute_entry(
        binding,
        wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
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
    pass: &ShaderPass,
    groups: &[&wgpu::BindGroup],
    size: (u32, u32),
) {
    dispatch_layers(encoder, label, pass, groups, size, 1);
}

pub(crate) fn dispatch_layers(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    pass: &ShaderPass,
    groups: &[&wgpu::BindGroup],
    size: (u32, u32),
    layers: u32,
) {
    let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    });
    compute.set_pipeline(&pass.pipeline);
    for (index, group) in (0..).zip(groups) {
        compute.set_bind_group(index, *group, &[]);
    }
    compute.dispatch_workgroups(
        size.0.div_ceil(pass.workgroup.0),
        size.1.div_ceil(pass.workgroup.1),
        layers,
    );
}
