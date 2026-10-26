#[cfg(test)]
mod tests;

use std::ops::RangeInclusive;

use rusplay_core::{HdrLevels, MatrixCoefficients, TransferFunction};

use crate::{
    LineariseSettings, OutputLight, Reconfigured, ScaleError, SourceChannels,
    color::{ColorMapSettings, ColorPlan, MapperBuffers, MappingPath, float},
    gpu::{self, LUT_FORMAT, ScalePipelines, buffer_group, uniform_buffer},
};

pub const DEFAULT_LUT_SIZE: u32 = 65;
pub const LUT_SIZES: RangeInclusive<u32> = 2..=256;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorLutSettings {
    pub transfer: TransferFunction,
    pub hdr: HdrLevels,
    pub matrix: MatrixCoefficients,
    pub map: Option<ColorMapSettings>,
    pub size: u32,
}

impl ColorLutSettings {
    fn linearise(&self) -> LineariseSettings {
        LineariseSettings {
            channels: SourceChannels::RGBA,
            transfer: self.transfer,
            hdr: self.hdr,
            matrix: self.matrix,
            output: OutputLight::Linear,
        }
    }

    fn plan(&self, device: &wgpu::Device) -> Result<LutPlan, ScaleError> {
        if !LUT_SIZES.contains(&self.size) || self.size > device.limits().max_texture_dimension_3d {
            return Err(ScaleError::LutSizeOutOfRange(self.size));
        }
        self.linearise().validate()?;

        let mut plan = ColorPlan::new(&self.map.unwrap_or_default())?;
        if plan.tone.is_some_and(|stage| stage.dynamic_peak.is_some()) {
            return Err(ScaleError::LutDynamicPeak);
        }
        let clamped = self.map.is_some() && plan.path != MappingPath::Matrix;
        match plan.path {
            MappingPath::Highlight => return Err(ScaleError::LutHighlightMapping),
            MappingPath::Clip => plan.path = MappingPath::Matrix,
            MappingPath::Matrix | MappingPath::Perceptual | MappingPath::Desaturate => {}
        }

        Ok(LutPlan {
            clamped,
            toe: !self.transfer.is_hdr()
                || (self.map.is_some() && plan.path == MappingPath::Matrix),
            peak: plan.target_peak,
            bake: plan,
        })
    }

    fn bake_bytes(&self, plan: &LutPlan) -> Vec<u8> {
        let linearise = self.linearise();
        let luma = self.matrix.rows()[0];
        let mut words = Vec::with_capacity(BAKE_WORDS);

        words.extend(luma.map(float));
        words.push(0);
        words.extend([
            float(linearise.hdr_scale()),
            float(linearise.hlg_exponent()),
            0,
            0,
        ]);
        words.extend([
            linearise.transfer_code(),
            u32::from(self.map.is_some()),
            u32::from(plan.toe),
            0,
        ]);

        words.iter().flat_map(|word| word.to_ne_bytes()).collect()
    }
}

struct LutPlan {
    bake: ColorPlan,
    clamped: bool,
    toe: bool,
    peak: f64,
}

impl LutPlan {
    fn decoding_bytes(&self) -> Vec<u8> {
        [
            u32::from(self.clamped),
            float(self.peak),
            u32::from(self.toe),
            0,
        ]
        .iter()
        .flat_map(|word| word.to_ne_bytes())
        .collect()
    }
}

const BAKE_WORDS: usize = 12;

#[derive(Debug)]
struct LutResources {
    view: wgpu::TextureView,
    decoding: wgpu::Buffer,
    store_group: wgpu::BindGroup,
    bake_group: wgpu::BindGroup,
    mapper: MapperBuffers,
    baked: bool,
}

impl LutResources {
    fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: &ColorLutSettings,
        plan: &LutPlan,
    ) -> Self {
        let view = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("rusplay-color-lut"),
                size: wgpu::Extent3d {
                    width: settings.size,
                    height: settings.size,
                    depth_or_array_layers: settings.size,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: LUT_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bake = uniform_buffer(device, "rusplay-color-lut-bake", &settings.bake_bytes(plan));

        Self {
            store_group: gpu::view_group(device, &pipelines.color.lut_store_layout, &view),
            bake_group: buffer_group(
                device,
                "rusplay-color-lut-bake",
                &pipelines.color.lut_bake_layout,
                &[&bake],
            ),
            decoding: uniform_buffer(device, "rusplay-color-lut-decoding", &plan.decoding_bytes()),
            mapper: MapperBuffers::new(pipelines, device, &plan.bake),
            view,
            baked: false,
        }
    }
}

#[derive(Debug)]
pub struct ColorLut {
    pipelines: ScalePipelines,
    settings: ColorLutSettings,
    resources: LutResources,
}

impl ColorLut {
    pub fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: ColorLutSettings,
    ) -> Result<Self, ScaleError> {
        let plan = settings.plan(device)?;

        Ok(Self {
            pipelines: pipelines.clone(),
            resources: LutResources::new(pipelines, device, &settings, &plan),
            settings,
        })
    }

    pub fn settings(&self) -> ColorLutSettings {
        self.settings
    }

    pub fn configure(
        &mut self,
        device: &wgpu::Device,
        settings: ColorLutSettings,
    ) -> Result<Reconfigured, ScaleError> {
        if settings == self.settings {
            return Ok(Reconfigured::Unchanged);
        }
        let plan = settings.plan(device)?;

        self.resources = LutResources::new(&self.pipelines, device, &settings, &plan);
        self.settings = settings;
        Ok(Reconfigured::Tables)
    }

    pub fn encode_bake(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if self.resources.baked {
            return;
        }

        let size = self.settings.size;
        gpu::dispatch_layers(
            encoder,
            "rusplay-color-lut-bake",
            &self.pipelines.color.lut_bake,
            &[
                &self.resources.store_group,
                &self.resources.mapper.map_group,
                &self.resources.bake_group,
            ],
            (size, size),
            size,
        );
        self.resources.baked = true;
    }

    pub(crate) fn view(&self) -> &wgpu::TextureView {
        &self.resources.view
    }

    pub(crate) fn decoding(&self) -> &wgpu::Buffer {
        &self.resources.decoding
    }
}
