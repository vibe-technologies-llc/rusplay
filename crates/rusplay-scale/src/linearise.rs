#[cfg(test)]
mod tests;

use rusplay_core::{HdrLevels, MatrixCoefficients, TransferFunction};

use crate::{
    OutputLight, Reconfigured, ScaleError,
    convert::{HLG_GAMMA_PER_DECADE, HLG_REFERENCE_GAMMA, HLG_REFERENCE_PEAK_NITS},
    gpu::{self, ScalePipelines, buffer_group, uniform_buffer, view_group},
    scaler::GroupCache,
};

const TRANSFER_NONE: u32 = 0;
const TRANSFER_SRGB: u32 = 1;
const TRANSFER_BT709: u32 = 2;
const TRANSFER_PQ: u32 = 3;
const TRANSFER_HLG: u32 = 4;
const TRANSFER_GAMMA22: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChannelOrder {
    #[default]
    Rgba,
    Bgra,
    Argb,
    Abgr,
}

impl ChannelOrder {
    pub const fn lanes(self) -> [u32; 4] {
        match self {
            Self::Rgba => [0, 1, 2, 3],
            Self::Bgra => [2, 1, 0, 3],
            Self::Argb => [1, 2, 3, 0],
            Self::Abgr => [3, 2, 1, 0],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TexelPacking {
    #[default]
    Float,
    Rgb10Alpha2Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SourceChannels {
    pub packing: TexelPacking,
    pub order: ChannelOrder,
    pub opaque: bool,
}

impl SourceChannels {
    pub const RGBA: Self = Self::float(ChannelOrder::Rgba, false);
    pub const RGBX: Self = Self::float(ChannelOrder::Rgba, true);

    pub const fn float(order: ChannelOrder, opaque: bool) -> Self {
        Self {
            packing: TexelPacking::Float,
            order,
            opaque,
        }
    }

    pub const fn packed(order: ChannelOrder, opaque: bool) -> Self {
        Self {
            packing: TexelPacking::Rgb10Alpha2Low,
            order,
            opaque,
        }
    }

    pub const fn reads_as_rgba(self) -> bool {
        matches!(self.packing, TexelPacking::Float) && matches!(self.order, ChannelOrder::Rgba)
    }

    const fn accepts(self, sample: Option<wgpu::TextureSampleType>) -> bool {
        matches!(
            (self.packing, sample),
            (
                TexelPacking::Float,
                Some(wgpu::TextureSampleType::Float { .. })
            ) | (
                TexelPacking::Rgb10Alpha2Low,
                Some(wgpu::TextureSampleType::Uint)
            )
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineariseSettings {
    pub channels: SourceChannels,
    pub transfer: TransferFunction,
    pub hdr: HdrLevels,
    pub matrix: MatrixCoefficients,
    pub output: OutputLight,
}

impl LineariseSettings {
    pub fn encoded(channels: SourceChannels) -> Self {
        Self {
            channels,
            transfer: TransferFunction::Linear,
            hdr: HdrLevels::default(),
            matrix: MatrixCoefficients::default(),
            output: OutputLight::Encoded,
        }
    }

    fn validate(&self) -> Result<(), ScaleError> {
        if !self.transfer.is_hdr() || self.output == OutputLight::Encoded {
            return Ok(());
        }
        if !self.hdr.reference_white_in_range() {
            return Err(ScaleError::ReferenceWhiteOutOfRange(
                self.hdr.reference_white_nits,
            ));
        }
        if !self.hdr.peak_in_range() {
            return Err(ScaleError::PeakLuminanceOutOfRange(self.hdr.peak_nits));
        }

        Ok(())
    }

    const fn transfer_code(&self) -> u32 {
        match (self.output, self.transfer) {
            (OutputLight::Encoded, _) | (OutputLight::Linear, TransferFunction::Linear) => {
                TRANSFER_NONE
            }
            (OutputLight::Linear, TransferFunction::Srgb) => TRANSFER_SRGB,
            (OutputLight::Linear, TransferFunction::Bt709) => TRANSFER_BT709,
            (OutputLight::Linear, TransferFunction::Pq) => TRANSFER_PQ,
            (OutputLight::Linear, TransferFunction::Hlg) => TRANSFER_HLG,
            (OutputLight::Linear, TransferFunction::Gamma22) => TRANSFER_GAMMA22,
        }
    }

    pub(crate) fn hdr_scale(&self) -> f64 {
        let white = f64::from(self.hdr.reference_white_nits);
        match self.transfer {
            TransferFunction::Hlg => f64::from(self.hdr.peak_nits) / white,
            _ => f64::from(HdrLevels::MAX_NITS) / white,
        }
    }

    pub(crate) fn hlg_exponent(&self) -> f64 {
        let peak = f64::from(self.hdr.peak_nits);

        HLG_GAMMA_PER_DECADE * (peak / HLG_REFERENCE_PEAK_NITS).log10() + HLG_REFERENCE_GAMMA - 1.0
    }

    fn params_bytes(&self) -> Vec<u8> {
        let lanes = self.channels.order.lanes();
        let luma = self.matrix.rows()[0];
        let mut words = Vec::with_capacity(PARAMS_WORDS);

        words.extend(lanes);
        words.extend(luma.map(float));
        words.push(0);
        words.extend([float(self.hdr_scale()), float(self.hlg_exponent()), 0, 0]);
        words.extend([self.transfer_code(), u32::from(self.channels.opaque), 0, 0]);

        words.iter().flat_map(|word| word.to_ne_bytes()).collect()
    }
}

const PARAMS_WORDS: usize = 16;

fn float(value: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let narrowed = value as f32;
    narrowed.to_bits()
}

#[derive(Debug)]
pub struct Lineariser {
    pipelines: ScalePipelines,
    settings: LineariseSettings,
    params: wgpu::BindGroup,
    sources: GroupCache<wgpu::TextureView>,
    destinations: GroupCache<wgpu::TextureView>,
}

impl Lineariser {
    pub fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: LineariseSettings,
    ) -> Result<Self, ScaleError> {
        settings.validate()?;

        Ok(Self {
            pipelines: pipelines.clone(),
            params: params_group(pipelines, device, &settings),
            settings,
            sources: GroupCache::default(),
            destinations: GroupCache::default(),
        })
    }

    pub fn settings(&self) -> LineariseSettings {
        self.settings
    }

    pub fn configure(
        &mut self,
        device: &wgpu::Device,
        settings: LineariseSettings,
    ) -> Result<Reconfigured, ScaleError> {
        if settings == self.settings {
            return Ok(Reconfigured::Unchanged);
        }
        settings.validate()?;

        if settings.channels.packing != self.settings.channels.packing {
            self.sources = GroupCache::default();
        }
        self.params = params_group(&self.pipelines, device, &settings);
        self.settings = settings;
        Ok(Reconfigured::Parameters)
    }

    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        let format = source.texture().format();
        if !self
            .settings
            .channels
            .accepts(format.sample_type(None, None))
        {
            return Err(ScaleError::LineariseSourceFormat {
                format,
                packing: self.settings.channels.packing,
            });
        }

        let pipelines = &self.pipelines;
        let passes = &pipelines.linearise;
        let (layout, pass) = match self.settings.channels.packing {
            TexelPacking::Float => (&pipelines.source_layout, &passes.float),
            TexelPacking::Rgb10Alpha2Low => (&passes.packed_source_layout, &passes.packed),
        };
        let source_group = self
            .sources
            .get_or_create(source, || view_group(device, layout, source));
        let target_group = self
            .destinations
            .get_or_create(target, || pipelines.destination_group(device, target));
        let size = target.texture().size();

        gpu::dispatch(
            encoder,
            "rusplay-linearise",
            pass,
            &[&source_group, &target_group, &self.params],
            (size.width, size.height),
        );
        Ok(())
    }
}

fn params_group(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    settings: &LineariseSettings,
) -> wgpu::BindGroup {
    let params = uniform_buffer(device, "rusplay-linearise-params", &settings.params_bytes());

    buffer_group(
        device,
        "rusplay-linearise-params",
        &pipelines.linearise.params_layout,
        &[&params],
    )
}
