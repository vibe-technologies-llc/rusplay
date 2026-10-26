use rusplay_config::{ColorConfig, ColorMethod};
use rusplay_core::{ColorSpace, HdrLevels, TransferFunction};
use rusplay_scale::{
    ColorChain, ColorChainSettings, ColorLut, ColorLutSettings, ColorMapSettings, ColorMapper,
    ColorPlan, ColorVolume, DynamicPeak, GamutMapping, LineariseSettings, Lineariser, MappingPath,
    OutputLight, ScalePipelines, SourceChannels, ToneMapping,
};

use crate::{ClientError, FrameColor};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MappingOptions {
    pub tone: ToneMapping,
    pub gamut: GamutMapping,
    pub processing: Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Processing {
    #[cfg(test)]
    Separate,
    Fused,
    Lut(u32),
}

impl From<&ColorConfig> for MappingOptions {
    fn from(color: &ColorConfig) -> Self {
        let tone = &color.tone_mapping;
        let gamut = &color.gamut_mapping;

        Self {
            tone: ToneMapping {
                operator: tone.operator,
                dynamic_peak: tone.dynamic_peak.then(|| DynamicPeak {
                    percentile: f64::from(tone.peak_percentile),
                    smoothing_frames: tone.smoothing_frames,
                    scene_threshold_db: f64::from(tone.scene_threshold_db),
                }),
            },
            gamut: GamutMapping {
                mode: gamut.mode,
                knee: f64::from(gamut.knee),
            },
            processing: match color.processing.method {
                ColorMethod::Analytic => Processing::Fused,
                ColorMethod::Lut => Processing::Lut(color.processing.lut_size),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ColorRoute {
    pub linearise: Option<LineariseSettings>,
    pub map: Option<ColorMapSettings>,
    pub chain: Option<ColorChainSettings>,
    pub lut: Option<ColorLutSettings>,
    pub unpack: OutputLight,
    pub output: FrameColor,
}

impl ColorRoute {
    pub(crate) fn plan(
        source: FrameColor,
        target: ColorVolume,
        options: &MappingOptions,
    ) -> Result<Self, ClientError> {
        let space = rendered_space(source.space, &target);
        let linearise = (source.light == OutputLight::Encoded).then_some(LineariseSettings {
            channels: SourceChannels::RGBA,
            transfer: space.transfer,
            hdr: space.hdr,
            matrix: source.matrix,
            output: OutputLight::Linear,
        });
        let settings = ColorMapSettings {
            source: ColorVolume::of_content(&space),
            target,
            tone: options.tone,
            gamut: options.gamut,
        };
        let plan = ColorPlan::new(&settings)?;
        let map = (!plan.is_identity()).then_some(settings);
        let output = source.mapped_to(&target);
        let fusable = plan.tone().is_none_or(|stage| stage.dynamic_peak.is_none());
        let bakeable = fusable && plan.path() != MappingPath::Highlight;
        let colour_passes = linearise.is_some() || map.is_some();
        let chain = ColorChainSettings {
            transfer: space.transfer,
            hdr: space.hdr,
            matrix: source.matrix,
            map,
        };
        let separate = Self {
            linearise,
            map,
            chain: None,
            lut: None,
            unpack: source.light,
            output,
        };
        let fused = Self {
            linearise: None,
            map: None,
            unpack: OutputLight::Encoded,
            ..separate
        };

        Ok(match options.processing {
            Processing::Lut(size) if bakeable && colour_passes => Self {
                lut: Some(ColorLutSettings {
                    transfer: chain.transfer,
                    hdr: chain.hdr,
                    matrix: chain.matrix,
                    map: chain.map,
                    size,
                }),
                ..fused
            },
            Processing::Fused if fusable && colour_passes => Self {
                chain: Some(chain),
                ..fused
            },
            _ => separate,
        })
    }

    pub(crate) fn passes(&self) -> usize {
        usize::from(self.linearise.is_some()) + usize::from(self.map.is_some())
    }
}

fn rendered_space(mut space: ColorSpace, target: &ColorVolume) -> ColorSpace {
    if space.transfer == TransferFunction::Hlg && target.is_hdr() {
        space.hdr.peak_nits = whole_nits(target.peak_nits);
    }
    space
}

pub(crate) fn whole_nits(nits: f64) -> u32 {
    let clamped = nits.round().clamp(1.0, f64::from(HdrLevels::MAX_NITS));

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = clamped as u32;
    whole
}

pub(crate) struct ColorStage {
    pipelines: ScalePipelines,
    options: MappingOptions,
    planned: Option<(FrameColor, ColorVolume, ColorRoute)>,
    lineariser: Option<Lineariser>,
    mapper: Option<ColorMapper>,
    chain: Option<ColorChain>,
    lut: Option<ColorLut>,
}

pub(crate) enum FusedColor<'a> {
    Chain(&'a mut ColorChain),
    Lut(&'a mut ColorLut),
}

impl ColorStage {
    pub(crate) fn new(pipelines: &ScalePipelines, options: MappingOptions) -> Self {
        Self {
            pipelines: pipelines.clone(),
            options,
            planned: None,
            lineariser: None,
            mapper: None,
            chain: None,
            lut: None,
        }
    }

    pub(crate) fn route(
        &mut self,
        device: &wgpu::Device,
        source: FrameColor,
        target: ColorVolume,
    ) -> Result<ColorRoute, ClientError> {
        if let Some((color, volume, route)) = self.planned
            && color == source
            && volume == target
        {
            return Ok(route);
        }

        let route = ColorRoute::plan(source, target, &self.options)?;
        if let Some(settings) = route.linearise {
            match &mut self.lineariser {
                Some(lineariser) => {
                    lineariser.configure(device, settings)?;
                }
                None => {
                    self.lineariser = Some(Lineariser::new(&self.pipelines, device, settings)?);
                }
            }
        }
        if let Some(settings) = route.map {
            match &mut self.mapper {
                Some(mapper) => {
                    mapper.configure(device, settings)?;
                }
                None => self.mapper = Some(ColorMapper::new(&self.pipelines, device, settings)?),
            }
        }
        if let Some(settings) = route.chain {
            match &mut self.chain {
                Some(chain) => {
                    chain.configure(device, settings)?;
                }
                None => self.chain = Some(ColorChain::new(&self.pipelines, device, settings)?),
            }
        }
        if let Some(settings) = route.lut {
            match &mut self.lut {
                Some(lut) => {
                    lut.configure(device, settings)?;
                }
                None => self.lut = Some(ColorLut::new(&self.pipelines, device, settings)?),
            }
        }
        self.planned = Some((source, target, route));

        Ok(route)
    }

    pub(crate) fn linearise(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) -> Result<(), ClientError> {
        match &mut self.lineariser {
            Some(lineariser) => Ok(lineariser.encode(device, encoder, source, target)?),
            None => Ok(()),
        }
    }

    pub(crate) fn map(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        if let Some(mapper) = &mut self.mapper {
            mapper.encode(device, encoder, source, target);
        }
    }

    pub(crate) fn fused(&mut self, route: &ColorRoute) -> Option<FusedColor<'_>> {
        if route.chain.is_some() {
            return self.chain.as_mut().map(FusedColor::Chain);
        }
        route.lut.and(self.lut.as_mut()).map(FusedColor::Lut)
    }

    #[cfg(test)]
    pub(crate) fn mapper(&self) -> Option<&ColorMapper> {
        self.mapper.as_ref()
    }
}

#[cfg(test)]
mod tests;
