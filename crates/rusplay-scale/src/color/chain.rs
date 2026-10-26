#[cfg(test)]
mod tests;

use rusplay_core::{HdrLevels, MatrixCoefficients, TransferFunction};

use crate::{
    LineariseSettings, OutputLight, Reconfigured, ScaleError, SourceChannels,
    color::{ColorMapSettings, ColorPlan, MapperBuffers, float},
    gpu::{ScalePipelines, uniform_buffer},
};

const SIGNAL_WORDS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorChainSettings {
    pub transfer: TransferFunction,
    pub hdr: HdrLevels,
    pub matrix: MatrixCoefficients,
    pub map: Option<ColorMapSettings>,
}

impl ColorChainSettings {
    pub(crate) fn linearise(&self) -> LineariseSettings {
        LineariseSettings {
            channels: SourceChannels::RGBA,
            transfer: self.transfer,
            hdr: self.hdr,
            matrix: self.matrix,
            output: OutputLight::Linear,
        }
    }

    pub(crate) fn signal_bytes(&self, toe: bool) -> Vec<u8> {
        let linearise = self.linearise();
        let luma = self.matrix.rows()[0];
        let mut words = Vec::with_capacity(SIGNAL_WORDS);

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
            u32::from(toe),
            0,
        ]);

        words.iter().flat_map(|word| word.to_ne_bytes()).collect()
    }

    fn plan(&self) -> Result<ColorPlan, ScaleError> {
        self.linearise().validate()?;

        let plan = ColorPlan::new(&self.map.unwrap_or_default())?;
        if plan.tone.is_some_and(|stage| stage.dynamic_peak.is_some()) {
            return Err(ScaleError::ChainDynamicPeak);
        }
        Ok(plan)
    }
}

#[derive(Debug)]
struct ChainResources {
    plan: ColorPlan,
    signal: wgpu::Buffer,
    mapper: MapperBuffers,
}

impl ChainResources {
    fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: &ColorChainSettings,
    ) -> Result<Self, ScaleError> {
        let plan = settings.plan()?;

        Ok(Self {
            signal: uniform_buffer(device, "rusplay-color-chain", &settings.signal_bytes(false)),
            mapper: MapperBuffers::new(pipelines, device, &plan),
            plan,
        })
    }
}

#[derive(Debug)]
pub struct ColorChain {
    pipelines: ScalePipelines,
    settings: ColorChainSettings,
    resources: ChainResources,
}

impl ColorChain {
    pub fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: ColorChainSettings,
    ) -> Result<Self, ScaleError> {
        Ok(Self {
            pipelines: pipelines.clone(),
            resources: ChainResources::new(pipelines, device, &settings)?,
            settings,
        })
    }

    pub fn settings(&self) -> ColorChainSettings {
        self.settings
    }

    pub fn plan(&self) -> &ColorPlan {
        &self.resources.plan
    }

    pub fn configure(
        &mut self,
        device: &wgpu::Device,
        settings: ColorChainSettings,
    ) -> Result<Reconfigured, ScaleError> {
        if settings == self.settings {
            return Ok(Reconfigured::Unchanged);
        }

        self.resources = ChainResources::new(&self.pipelines, device, &settings)?;
        self.settings = settings;
        Ok(Reconfigured::Tables)
    }

    pub fn encode_tables(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.resources
            .mapper
            .encode_boundary(&self.pipelines, encoder);
    }

    pub(crate) fn target_group(
        &self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        target: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let mapper = &self.resources.mapper;

        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rusplay-unpack-chain-target"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(target),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: mapper.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: mapper.tone_state.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&mapper.boundary),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.resources.signal.as_entire_binding(),
                },
            ],
        })
    }

    pub(crate) fn identity(&self) -> &wgpu::TextureView {
        &self.resources.mapper.boundary
    }
}
