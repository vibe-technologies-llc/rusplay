use rusplay_core::{
    ChromaSubsampling, ColorRange, MatrixCoefficients, PixelFormat, Resolution, ScaleFilter,
    TransferFunction,
};

use crate::{
    ChromaSiting, ColorChain, ColorLut, DebandSettings, DmaBufTexture, FrameLayout, Geometry,
    Kernel, PlaneContent, PlaneLayout, Quantization, Reconfigured, ScaleError, TapTable, YuvFrame,
    convert::chroma_size,
    deband,
    gpu::{
        self, CHROMA_FORMAT, DEBANDED_LUMA_FORMAT, ScalePipelines, UnpackOutput, UnpackPasses,
        UnpackSource, buffer_group, storage_buffer, uniform_buffer,
    },
    polar::PolarGeometry,
    scaler::{GroupCache, resolution_pair},
};

mod phase;

use phase::PhaseKernels;

const WORD_BYTES: u64 = 4;
const LEFT_SITING_UPSAMPLE_PHASE: f64 = 0.25;
const UPSAMPLING_RATIO: f32 = 0.5;
const ANTIRINGING_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;
const TRANSFER_NONE: u32 = 0;
const TRANSFER_SRGB: u32 = 1;
const TRANSFER_BT709: u32 = 2;
const TRANSFER_GAMMA22: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputLight {
    Linear,
    Encoded,
}

impl OutputLight {
    pub const fn linearizable(transfer: TransferFunction) -> Self {
        match transfer {
            TransferFunction::Srgb
            | TransferFunction::Bt709
            | TransferFunction::Gamma22
            | TransferFunction::Linear => Self::Linear,
            TransferFunction::Pq | TransferFunction::Hlg => Self::Encoded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbConvertSettings {
    pub resolution: Resolution,
    pub format: PixelFormat,
    pub layout: PlaneLayout,
    pub matrix: MatrixCoefficients,
    pub range: ColorRange,
    pub transfer: TransferFunction,
    pub output: OutputLight,
    pub chroma_filter: ScaleFilter,
    pub chroma_siting: ChromaSiting,
    pub chroma_antiringing: f32,
    pub deband: Option<DebandSettings>,
    pub row_alignment: u32,
}

impl RgbConvertSettings {
    fn validate(&self) -> Result<(), ScaleError> {
        if self.resolution.pixel_count() == 0 {
            return Err(ScaleError::EmptySource(self.resolution));
        }
        if !ANTIRINGING_RANGE.contains(&self.chroma_antiringing) {
            return Err(ScaleError::AntiringingOutOfRange(self.chroma_antiringing));
        }
        if let Some(deband) = &self.deband {
            deband.validate()?;
        }

        self.transfer_code()?;
        Ok(())
    }

    fn transfer_code(&self) -> Result<u32, ScaleError> {
        match (self.output, self.transfer) {
            (OutputLight::Encoded, _) | (OutputLight::Linear, TransferFunction::Linear) => {
                Ok(TRANSFER_NONE)
            }
            (OutputLight::Linear, TransferFunction::Srgb) => Ok(TRANSFER_SRGB),
            (OutputLight::Linear, TransferFunction::Bt709) => Ok(TRANSFER_BT709),
            (OutputLight::Linear, TransferFunction::Gamma22) => Ok(TRANSFER_GAMMA22),
            (OutputLight::Linear, transfer @ (TransferFunction::Pq | TransferFunction::Hlg)) => {
                Err(ScaleError::LinearOutputTransferUnsupported(transfer))
            }
        }
    }

    pub fn frame_layout(&self) -> Result<FrameLayout, ScaleError> {
        FrameLayout::packed(
            self.resolution,
            self.format,
            self.layout,
            self.row_alignment,
            1,
        )
    }

    pub fn chroma_size(&self) -> (u32, u32) {
        chroma_size(self.resolution, self.format.chroma)
    }

    pub(crate) fn polar_chroma(&self) -> bool {
        Kernel::new(self.chroma_filter).geometry() == Geometry::Polar
            && self.format.chroma != ChromaSubsampling::Yuv444
    }

    fn siting_phase(&self) -> f64 {
        match self.chroma_siting {
            ChromaSiting::Left => LEFT_SITING_UPSAMPLE_PHASE,
            ChromaSiting::Center => 0.0,
        }
    }

    pub(crate) fn polar_chroma_geometry(&self) -> PolarGeometry {
        #[allow(clippy::cast_possible_truncation)]
        let phase = self.siting_phase() as f32;

        let vertical_ratio = match self.format.chroma {
            ChromaSubsampling::Yuv420 => UPSAMPLING_RATIO,
            ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444 => 1.0,
        };
        let geometry = PolarGeometry::with_ratio(
            self.chroma_filter,
            self.chroma_size(),
            resolution_pair(self.resolution),
            (UPSAMPLING_RATIO, vertical_ratio),
            (phase, 0.0),
        );

        match self.format.chroma {
            ChromaSubsampling::Yuv420 => geometry,
            ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444 => {
                geometry.with_identity_vertical()
            }
        }
    }

    pub(crate) fn chroma_tables(&self) -> (TapTable, TapTable) {
        let kernel = Kernel::new(self.chroma_filter);
        let (width, height) = resolution_pair(self.resolution);
        let (chroma_width, chroma_height) = self.chroma_size();
        let phase = self.siting_phase();

        if self.polar_chroma() {
            return (TapTable::identity(width), TapTable::identity(height));
        }

        let horizontal = match self.format.chroma {
            ChromaSubsampling::Yuv444 => TapTable::identity(width),
            ChromaSubsampling::Yuv420 | ChromaSubsampling::Yuv422 => {
                TapTable::build_with_phase(kernel, chroma_width, 2 * chroma_width, phase)
            }
        };
        let vertical = match self.format.chroma {
            ChromaSubsampling::Yuv420 => TapTable::build(kernel, chroma_height, 2 * chroma_height),
            ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444 => TapTable::identity(height),
        };

        (horizontal, vertical)
    }
}

#[derive(Debug)]
pub struct RgbConverter {
    pipelines: ScalePipelines,
    settings: RgbConvertSettings,
    layout: FrameLayout,
    plan: UnpackPlan,
    inputs: GroupCache<wgpu::Buffer>,
    planes: GroupCache<(wgpu::TextureView, wgpu::TextureView)>,
    targets: GroupCache<wgpu::TextureView>,
    lut_targets: GroupCache<(wgpu::TextureView, wgpu::TextureView)>,
    chain_targets: GroupCache<(wgpu::TextureView, wgpu::TextureView)>,
    staged_luma: Option<PlaneCopy>,
    staged_chroma: Option<PlaneCopy>,
}

#[derive(Debug)]
struct PlaneCopy {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
}

impl RgbConverter {
    pub fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: RgbConvertSettings,
    ) -> Result<Self, ScaleError> {
        settings.validate()?;
        let layout = settings.frame_layout()?;

        Ok(Self {
            pipelines: pipelines.clone(),
            plan: UnpackPlan::new(pipelines, device, &settings, &layout)?,
            settings,
            layout,
            inputs: GroupCache::default(),
            planes: GroupCache::default(),
            targets: GroupCache::default(),
            lut_targets: GroupCache::default(),
            chain_targets: GroupCache::default(),
            staged_luma: None,
            staged_chroma: None,
        })
    }

    pub fn set_frame_capacity(&mut self, frames: usize) {
        self.inputs.set_capacity(frames);
        self.planes.set_capacity(frames);
        self.targets.set_capacity(frames);
        self.lut_targets.set_capacity(frames);
        self.chain_targets.set_capacity(frames);
    }

    pub fn settings(&self) -> RgbConvertSettings {
        self.settings
    }

    pub fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    pub fn create_frame(&self, device: &wgpu::Device) -> YuvFrame {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rusplay-yuv-upload"),
            size: self.layout.size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        YuvFrame::from_parts(buffer, self.layout.clone())
    }

    pub fn configure(
        &mut self,
        device: &wgpu::Device,
        settings: RgbConvertSettings,
    ) -> Result<Reconfigured, ScaleError> {
        if settings == self.settings {
            return Ok(Reconfigured::Unchanged);
        }
        settings.validate()?;
        let layout = settings.frame_layout()?;

        self.plan = UnpackPlan::new(&self.pipelines, device, &settings, &layout)?;
        self.settings = settings;
        self.layout = layout;
        self.staged_luma = None;
        self.staged_chroma = None;
        Ok(Reconfigured::Tables)
    }

    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &YuvFrame,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        self.encode_frame(device, encoder, frame, Fused::None, target)
    }

    pub fn encode_with_lut(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &YuvFrame,
        lut: &mut ColorLut,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        self.encode_frame(device, encoder, frame, Fused::Lut(lut), target)
    }

    pub fn encode_with_chain(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &YuvFrame,
        chain: &mut ColorChain,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        self.encode_frame(device, encoder, frame, Fused::Chain(chain), target)
    }

    pub fn encode_planes(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &DmaBufTexture,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        self.encode_dmabuf(device, encoder, source, Fused::None, target)
    }

    pub fn encode_planes_with_lut(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &DmaBufTexture,
        lut: &mut ColorLut,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        self.encode_dmabuf(device, encoder, source, Fused::Lut(lut), target)
    }

    pub fn encode_planes_with_chain(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &DmaBufTexture,
        chain: &mut ColorChain,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        self.encode_dmabuf(device, encoder, source, Fused::Chain(chain), target)
    }

    fn encode_frame(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &YuvFrame,
        fused: Fused,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        if *frame.layout() != self.layout {
            return Err(ScaleError::FrameLayoutMismatch);
        }

        let output = self.output(device, encoder, fused, target)?;
        let pipelines = &self.pipelines;
        let input_group = self.inputs.get_or_create(frame.buffer(), || {
            buffer_group(
                device,
                "rusplay-unpack-input",
                &pipelines.unpack.input_layout,
                &[frame.buffer()],
            )
        });

        self.plan.encode(
            &pipelines.unpack,
            (UnpackSource::Buffer, output.kind()),
            encoder,
            &input_group,
            output.group(),
        );
        Ok(())
    }

    fn encode_dmabuf(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &DmaBufTexture,
        fused: Fused,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        let mismatch = ScaleError::DmaBufFormatMismatch {
            fourcc: source.format().fourcc(),
            format: self.settings.format,
            layout: self.settings.layout,
        };
        if source.format().pixel_format() != Some(self.settings.format)
            || self.settings.layout != PlaneLayout::SemiPlanar
        {
            return Err(mismatch);
        }
        let (Some(luma_texture), Some(chroma_texture)) = (source.texture(0), source.texture(1))
        else {
            return Err(mismatch);
        };
        let (Some(luma), Some(chroma)) = (source.view(0), source.view(1)) else {
            return Err(mismatch);
        };

        let found = source.resolution();
        let needed = self.settings.resolution;
        if found.width < needed.width || found.height < needed.height {
            return Err(ScaleError::DmaBufTooSmall { found, needed });
        }

        let (luma, chroma) = if source.preserved()
            && luma_texture.usage().contains(wgpu::TextureUsages::COPY_SRC)
            && chroma_texture
                .usage()
                .contains(wgpu::TextureUsages::COPY_SRC)
        {
            let luma = stage_plane(device, encoder, luma_texture, &mut self.staged_luma);
            let chroma = stage_plane(device, encoder, chroma_texture, &mut self.staged_chroma);
            (luma, chroma)
        } else {
            (luma.clone(), chroma.clone())
        };

        let output = self.output(device, encoder, fused, target)?;
        let pipelines = &self.pipelines;
        let input_group = self
            .planes
            .get_or_create(&(luma.clone(), chroma.clone()), || {
                planes_group(device, &pipelines.unpack.planes_layout, &luma, &chroma)
            });

        self.plan.encode(
            &pipelines.unpack,
            (UnpackSource::Planes, output.kind()),
            encoder,
            &input_group,
            output.group(),
        );
        Ok(())
    }

    fn output(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        fused: Fused,
        target: &wgpu::TextureView,
    ) -> Result<Output, ScaleError> {
        let pipelines = &self.pipelines;
        match fused {
            Fused::None => {
                Ok(Output::Transfer(self.targets.get_or_create(target, || {
                    pipelines.destination_group(device, target)
                })))
            }
            Fused::Lut(lut) => {
                if self.settings.output != OutputLight::Encoded {
                    return Err(ScaleError::LutInputNotEncoded);
                }
                let baked_for = lut.settings().transfer;
                if baked_for != self.settings.transfer {
                    return Err(ScaleError::LutTransferMismatch {
                        lut: baked_for,
                        frame: self.settings.transfer,
                    });
                }

                lut.encode_bake(encoder);
                let key = (target.clone(), lut.view().clone());
                Ok(Output::Lut(self.lut_targets.get_or_create(&key, || {
                    lut_target_group(device, &pipelines.unpack.lut.target_layout, target, lut)
                })))
            }
            Fused::Chain(chain) => {
                if self.settings.output != OutputLight::Encoded {
                    return Err(ScaleError::ChainInputNotEncoded);
                }
                let decodes = chain.settings().transfer;
                if decodes != self.settings.transfer {
                    return Err(ScaleError::ChainTransferMismatch {
                        chain: decodes,
                        frame: self.settings.transfer,
                    });
                }

                chain.encode_tables(encoder);
                let key = (target.clone(), chain.identity().clone());
                Ok(Output::Chain(
                    self.chain_targets.get_or_create(&key, || {
                        chain.target_group(device, &pipelines.unpack.chain.target_layout, target)
                    }),
                ))
            }
        }
    }
}

enum Fused<'a> {
    None,
    Lut(&'a mut ColorLut),
    Chain(&'a mut ColorChain),
}

enum Output {
    Transfer(wgpu::BindGroup),
    Lut(wgpu::BindGroup),
    Chain(wgpu::BindGroup),
}

impl Output {
    const fn group(&self) -> &wgpu::BindGroup {
        match self {
            Self::Transfer(group) | Self::Lut(group) | Self::Chain(group) => group,
        }
    }

    const fn kind(&self) -> UnpackOutput {
        match self {
            Self::Transfer(_) => UnpackOutput::Transfer,
            Self::Lut(_) => UnpackOutput::Lut,
            Self::Chain(_) => UnpackOutput::Chain,
        }
    }
}

fn lut_target_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    target: &wgpu::TextureView,
    lut: &ColorLut,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rusplay-unpack-lut-target"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(target),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(lut.view()),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: lut.decoding().as_entire_binding(),
            },
        ],
    })
}

fn stage_plane(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    source: &wgpu::Texture,
    staged: &mut Option<PlaneCopy>,
) -> wgpu::TextureView {
    let size = source.size();
    let format = source.format();
    match staged.as_mut() {
        Some(plane) if plane.size == size && plane.format == format => {
            encoder.copy_texture_to_texture(
                source.as_image_copy(),
                plane.texture.as_image_copy(),
                size,
            );
            plane.view.clone()
        }
        _ => {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rusplay-decoded-plane"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            encoder.copy_texture_to_texture(source.as_image_copy(), texture.as_image_copy(), size);
            let stored = view.clone();
            *staged = Some(PlaneCopy {
                texture,
                view,
                size,
                format,
            });
            stored
        }
    }
}

fn planes_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    luma: &wgpu::TextureView,
    chroma: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rusplay-unpack-planes"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(luma),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(chroma),
            },
        ],
    })
}

#[derive(Debug)]
struct UnpackPlan {
    chroma_group: wgpu::BindGroup,
    rgb_group: wgpu::BindGroup,
    chroma_target: wgpu::BindGroup,
    chroma_source: wgpu::BindGroup,
    chroma_pass_size: (u32, u32),
    chroma_filter: ChromaFilterPass,
    rgb_pass_size: (u32, u32),
    deband: Option<DebandPlan>,
}

#[derive(Debug)]
struct DebandPlan {
    targets: wgpu::BindGroup,
    params: wgpu::BindGroup,
    input: wgpu::BindGroup,
    luma_size: (u32, u32),
    chroma_size: (u32, u32),
}

impl DebandPlan {
    fn new(
        passes: &UnpackPasses,
        device: &wgpu::Device,
        settings: &RgbConvertSettings,
        deband: &DebandSettings,
        unpacking: &wgpu::Buffer,
    ) -> Self {
        let luma_size = resolution_pair(settings.resolution);
        let chroma_size = settings.chroma_size();
        let luma = gpu::storage_texture(
            device,
            "rusplay-deband-luma",
            luma_size,
            DEBANDED_LUMA_FORMAT,
        );
        let chroma =
            gpu::storage_texture(device, "rusplay-deband-chroma", chroma_size, CHROMA_FORMAT);
        let deband_params = uniform_buffer(
            device,
            "rusplay-deband-params",
            &deband::params_bytes(deband, settings.range, settings.format.chroma),
        );

        Self {
            targets: views_group(
                device,
                "rusplay-deband-target",
                &passes.deband.target_layout,
                [&luma, &chroma],
            ),
            params: buffer_group(
                device,
                "rusplay-deband-params",
                &passes.deband.params_layout,
                &[unpacking, &deband_params],
            ),
            input: views_group(
                device,
                "rusplay-unpack-debanded",
                &passes.debanded_layout,
                [&luma, &chroma],
            ),
            luma_size,
            chroma_size,
        }
    }

    fn encode(
        &self,
        passes: &UnpackPasses,
        source: UnpackSource,
        encoder: &mut wgpu::CommandEncoder,
        input_group: &wgpu::BindGroup,
    ) {
        let Some(planes) = passes.deband.of(source) else {
            return;
        };
        let groups = [input_group, &self.targets, &self.params];

        gpu::dispatch(
            encoder,
            "rusplay-deband-luma",
            &planes.luma,
            &groups,
            self.luma_size,
        );
        gpu::dispatch(
            encoder,
            "rusplay-deband-chroma",
            &planes.chroma,
            &groups,
            self.chroma_size,
        );
    }
}

fn views_group(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    views: [&wgpu::TextureView; 2],
) -> wgpu::BindGroup {
    let [first, second] = views;
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(first),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(second),
            },
        ],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChromaFilterPass {
    Direct,
    Separable,
    Polar,
}

impl ChromaFilterPass {
    fn of(settings: &RgbConvertSettings) -> Self {
        if settings.chroma_size() == resolution_pair(settings.resolution) {
            Self::Direct
        } else if settings.polar_chroma() {
            Self::Polar
        } else {
            Self::Separable
        }
    }
}

impl UnpackPlan {
    fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: &RgbConvertSettings,
        layout: &FrameLayout,
    ) -> Result<Self, ScaleError> {
        let (horizontal, vertical) = settings.chroma_tables();
        let size = resolution_pair(settings.resolution);
        let chroma_filter = ChromaFilterPass::of(settings);
        let polar = (chroma_filter == ChromaFilterPass::Polar)
            .then(|| PhaseKernels::build(settings, gpu::POLAR_CHROMA_WORKGROUP));
        let chroma_pass_size = match chroma_filter {
            ChromaFilterPass::Polar => size,
            ChromaFilterPass::Direct | ChromaFilterPass::Separable => {
                (size.0, settings.chroma_size().1)
            }
        };

        let params = uniform_buffer(
            device,
            "rusplay-unpack-params",
            &params_bytes(settings, layout, (&horizontal, &vertical), polar.as_ref())?,
        );
        let (horizontal_firsts, horizontal_weights) = match &polar {
            Some(kernels) => (
                storage_buffer(device, "rusplay-unpack-phase-offsets", &kernels.offsets),
                storage_buffer(device, "rusplay-unpack-phase-weights", &kernels.weights),
            ),
            None => (
                storage_buffer(device, "rusplay-unpack-firsts", &horizontal.firsts),
                storage_buffer(device, "rusplay-unpack-weights", &horizontal.weights),
            ),
        };
        let horizontal_anchors =
            storage_buffer(device, "rusplay-unpack-anchors", &horizontal.anchors);
        let vertical_firsts = storage_buffer(device, "rusplay-unpack-firsts", &vertical.firsts);
        let vertical_weights = storage_buffer(device, "rusplay-unpack-weights", &vertical.weights);
        let vertical_anchors = storage_buffer(device, "rusplay-unpack-anchors", &vertical.anchors);
        let intermediate = gpu::storage_texture(
            device,
            "rusplay-unpack-chroma",
            chroma_pass_size,
            CHROMA_FORMAT,
        );
        let deband = settings
            .deband
            .as_ref()
            .map(|deband| DebandPlan::new(&pipelines.unpack, device, settings, deband, &params));

        Ok(Self {
            chroma_group: buffer_group(
                device,
                "rusplay-unpack-chroma",
                &pipelines.unpack.table_layout,
                &[
                    &params,
                    &horizontal_firsts,
                    &horizontal_weights,
                    &horizontal_anchors,
                ],
            ),
            rgb_group: buffer_group(
                device,
                "rusplay-unpack-rgb",
                &pipelines.unpack.table_layout,
                &[
                    &params,
                    &vertical_firsts,
                    &vertical_weights,
                    &vertical_anchors,
                ],
            ),
            chroma_target: gpu::view_group(
                device,
                &pipelines.convert.chroma_target_layout,
                &intermediate,
            ),
            chroma_source: pipelines.source_group(device, &intermediate),
            chroma_pass_size,
            chroma_filter,
            rgb_pass_size: size,
            deband,
        })
    }

    fn encode(
        &self,
        passes: &UnpackPasses,
        (source, output): (UnpackSource, UnpackOutput),
        encoder: &mut wgpu::CommandEncoder,
        input_group: &wgpu::BindGroup,
        target_group: &wgpu::BindGroup,
    ) {
        let (source, input_group) = match &self.deband {
            Some(deband) => {
                deband.encode(passes, source, encoder, input_group);
                (UnpackSource::Debanded, &deband.input)
            }
            None => (source, input_group),
        };
        let chroma_passes = passes.chroma(source);
        let rgb_pass = passes.rgb(source, output);
        let chroma_pass = match self.chroma_filter {
            ChromaFilterPass::Direct => None,
            ChromaFilterPass::Separable => Some(&chroma_passes.separable),
            ChromaFilterPass::Polar => Some(&chroma_passes.polar),
        };

        if let Some(chroma_pass) = chroma_pass {
            gpu::dispatch(
                encoder,
                "rusplay-unpack-chroma",
                chroma_pass,
                &[input_group, &self.chroma_target, &self.chroma_group],
                self.chroma_pass_size,
            );
        }
        gpu::dispatch(
            encoder,
            "rusplay-unpack-rgb",
            rgb_pass,
            &[
                input_group,
                &self.chroma_source,
                target_group,
                &self.rgb_group,
            ],
            self.rgb_pass_size,
        );
    }
}

fn params_bytes(
    settings: &RgbConvertSettings,
    layout: &FrameLayout,
    (horizontal, vertical): (&TapTable, &TapTable),
    polar: Option<&PhaseKernels>,
) -> Result<Vec<u8>, ScaleError> {
    let rows = settings.matrix.inverse_rows();
    let quantization = Quantization::new(settings.format.depth, settings.range);
    let (width, height) = resolution_pair(settings.resolution);
    let (chroma_width, chroma_height) = settings.chroma_size();
    let word = |bytes: u64| {
        u32::try_from(bytes / WORD_BYTES).map_err(|_| ScaleError::FrameTooLarge { bytes })
    };
    let float = |value: f64| {
        #[allow(clippy::cast_possible_truncation)]
        let narrowed = value as f32;
        narrowed.to_bits()
    };
    let luma = layout.planes[0];
    let chroma = layout.planes[1];
    let second_chroma = layout.planes.get(2).copied().unwrap_or(chroma);
    let horizontal_taps = polar.map_or(horizontal.taps_per_pixel, |kernels| kernels.taps_per_phase);
    let polar_words = polar.map_or([0; 9], |kernels| {
        [
            kernels.geometry.ratio.0.to_bits(),
            kernels.geometry.ratio.1.to_bits(),
            kernels.geometry.phase.0.to_bits(),
            kernels.geometry.phase.1.to_bits(),
            kernels.tile_offset.0.cast_unsigned(),
            kernels.tile_offset.1.cast_unsigned(),
            kernels.tile_size.0,
            kernels.tile_size.1,
            kernels.phase_rows,
        ]
    });

    let words = [
        float(rows[0][0]),
        float(rows[0][1]),
        float(rows[0][2]),
        0,
        float(rows[1][0]),
        float(rows[1][1]),
        float(rows[1][2]),
        0,
        float(rows[2][0]),
        float(rows[2][1]),
        float(rows[2][2]),
        0,
        float(quantization.luma.0),
        float(quantization.luma.1),
        float(quantization.chroma.0),
        float(quantization.chroma.1),
        width,
        height,
        chroma_width,
        chroma_height,
        settings.transfer_code()?,
        layout.bytes_per_sample,
        layout.sample_shift(),
        quantization.max_code,
        word(luma.offset)?,
        luma.stride / 4,
        word(chroma.offset)?,
        word(second_chroma.offset)?,
        chroma.stride / 4,
        u32::from(chroma.content == PlaneContent::CbCr),
        horizontal_taps,
        vertical.taps_per_pixel,
    ];
    let shaping = [settings.chroma_antiringing.to_bits(), 0, 0];

    Ok(words
        .iter()
        .chain(&polar_words)
        .chain(&shaping)
        .flat_map(|word| word.to_ne_bytes())
        .collect())
}

#[cfg(test)]
mod tests {
    mod deband;

    use rusplay_core::BitDepth;

    use super::*;
    use crate::{
        ColorConverter, ConvertSettings, InputLight, reference,
        testing::{self, Image, RGBA16F_TOLERANCE},
    };

    const ROUND_TRIP_EDGE_TOLERANCE: f32 = 0.03;
    const ROUND_TRIP_INTERIOR_TOLERANCE: f32 = 0.006;
    const ROUND_TRIP_BORDER: i64 = 4;
    const POLAR_FILTERS: [ScaleFilter; 6] = [
        ScaleFilter::EwaLanczos,
        ScaleFilter::EwaLanczosSharp,
        ScaleFilter::EwaGinseng,
        ScaleFilter::EwaHann,
        ScaleFilter::EwaRobidoux,
        ScaleFilter::EwaRobidouxSharp,
    ];

    fn interior_difference(actual: &Image, expected: &Image, border: i64) -> f32 {
        let (width, height) = (i64::from(actual.width), i64::from(actual.height));

        (border..height - border)
            .flat_map(|y| (border..width - border).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let (left, right) = (actual.texel(x, y), expected.texel(x, y));
                (0..3).map(move |channel| (left[channel] - right[channel]).abs())
            })
            .fold(0.0, f32::max)
    }

    fn settings(
        resolution: (u32, u32),
        chroma: ChromaSubsampling,
        depth: BitDepth,
        layout: PlaneLayout,
    ) -> RgbConvertSettings {
        RgbConvertSettings {
            resolution: Resolution::new(resolution.0, resolution.1),
            format: PixelFormat { chroma, depth },
            layout,
            matrix: MatrixCoefficients::Bt709,
            range: ColorRange::Limited,
            transfer: TransferFunction::Srgb,
            output: OutputLight::Linear,
            chroma_filter: ScaleFilter::Lanczos3,
            chroma_siting: ChromaSiting::Left,
            chroma_antiringing: 0.0,
            deband: None,
            row_alignment: 4,
        }
    }

    fn host_settings(settings: &RgbConvertSettings) -> ConvertSettings {
        ConvertSettings {
            resolution: settings.resolution,
            format: settings.format,
            layout: settings.layout,
            matrix: settings.matrix,
            range: settings.range,
            transfer: settings.transfer,
            hdr: rusplay_core::HdrLevels::default(),
            input: match settings.output {
                OutputLight::Linear => InputLight::Linear,
                OutputLight::Encoded => InputLight::Encoded,
            },
            chroma_filter: ScaleFilter::Hermite,
            chroma_siting: settings.chroma_siting,
            row_alignment: settings.row_alignment,
            extent_alignment: 1,
        }
    }

    fn encode_on_gpu(gpu: &testing::Gpu, image: &Image, settings: &RgbConvertSettings) -> Vec<u8> {
        let pipelines = ScalePipelines::new(&gpu.device);
        let mut converter =
            ColorConverter::new(&pipelines, &gpu.device, host_settings(settings)).unwrap();
        let frame = converter.create_frame(&gpu.device);
        let source = testing::upload(gpu, image);

        testing::submit(gpu, |encoder| {
            converter
                .encode(&gpu.device, encoder, &testing::view(&source), &frame)
                .unwrap();
        });
        testing::download_buffer(gpu, frame.buffer())
    }

    fn unpack_on_gpu(gpu: &testing::Gpu, bytes: &[u8], settings: RgbConvertSettings) -> Image {
        let pipelines = ScalePipelines::new(&gpu.device);
        let mut converter = RgbConverter::new(&pipelines, &gpu.device, settings).unwrap();
        let frame = converter.create_frame(&gpu.device);
        let (width, height) = resolution_pair(settings.resolution);
        let target = testing::storage_target(gpu, width, height);

        gpu.queue.write_buffer(frame.buffer(), 0, bytes);
        testing::submit(gpu, |encoder| {
            converter
                .encode(&gpu.device, encoder, &frame, &testing::view(&target))
                .unwrap();
        });
        testing::download(gpu, &target)
    }

    fn assert_matches_reference(settings: RgbConvertSettings) {
        let image = Image::pattern(settings.resolution.width, settings.resolution.height, 7);

        assert_image_matches_reference(&image, settings);
    }

    fn assert_image_matches_reference(image: &Image, settings: RgbConvertSettings) {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let bytes = encode_on_gpu(gpu, image, &settings);
        let actual = unpack_on_gpu(gpu, &bytes, settings);
        let expected = reference::unpack_rgb(&bytes, &settings);
        let difference = actual.max_difference(&expected);

        assert!(
            difference <= RGBA16F_TOLERANCE,
            "{settings:?}: max difference {difference}"
        );
    }

    #[test]
    fn every_format_matches_reference() {
        let chromas = [
            ChromaSubsampling::Yuv420,
            ChromaSubsampling::Yuv422,
            ChromaSubsampling::Yuv444,
        ];
        let depths = [BitDepth::Eight, BitDepth::Ten, BitDepth::Twelve];
        let layouts = [PlaneLayout::Planar, PlaneLayout::SemiPlanar];

        for (index, ((chroma, depth), layout)) in chromas
            .into_iter()
            .flat_map(|chroma| depths.map(|depth| (chroma, depth)))
            .flat_map(|pair| layouts.map(|layout| (pair, layout)))
            .enumerate()
        {
            assert_matches_reference(RgbConvertSettings {
                matrix: if index % 2 == 0 {
                    MatrixCoefficients::Bt709
                } else {
                    MatrixCoefficients::Bt2020Ncl
                },
                range: if index % 3 == 0 {
                    ColorRange::Full
                } else {
                    ColorRange::Limited
                },
                ..settings((37, 23), chroma, depth, layout)
            });
        }
    }

    #[test]
    fn chroma_filters_sitings_and_transfers_match_reference() {
        let base = settings(
            (64, 36),
            ChromaSubsampling::Yuv420,
            BitDepth::Ten,
            PlaneLayout::Planar,
        );

        for chroma_filter in [
            ScaleFilter::Bilinear,
            ScaleFilter::CatmullRom,
            ScaleFilter::Mitchell,
            ScaleFilter::Hermite,
        ] {
            assert_matches_reference(RgbConvertSettings {
                chroma_filter,
                chroma_siting: ChromaSiting::Center,
                ..base
            });
        }
        assert_matches_reference(RgbConvertSettings {
            transfer: TransferFunction::Bt709,
            ..base
        });
        assert_matches_reference(RgbConvertSettings {
            chroma_antiringing: 0.0,
            row_alignment: 256,
            ..base
        });
        assert_matches_reference(RgbConvertSettings {
            transfer: TransferFunction::Pq,
            output: OutputLight::Encoded,
            ..base
        });
    }

    #[test]
    fn polar_chroma_filters_match_reference() {
        let chromas = [ChromaSubsampling::Yuv420, ChromaSubsampling::Yuv422];
        let sitings = [ChromaSiting::Left, ChromaSiting::Center];

        for (index, chroma_filter) in POLAR_FILTERS.into_iter().enumerate() {
            for (chroma, chroma_siting) in chromas
                .into_iter()
                .flat_map(|chroma| sitings.map(|siting| (chroma, siting)))
            {
                assert_matches_reference(RgbConvertSettings {
                    chroma_filter,
                    chroma_siting,
                    layout: if index % 2 == 0 {
                        PlaneLayout::Planar
                    } else {
                        PlaneLayout::SemiPlanar
                    },
                    ..settings((37, 23), chroma, BitDepth::Ten, PlaneLayout::Planar)
                });
            }
        }
    }

    #[test]
    fn every_polar_footprint_fits_the_workgroup_tile() {
        for chroma_filter in POLAR_FILTERS {
            for chroma in [ChromaSubsampling::Yuv420, ChromaSubsampling::Yuv422] {
                for chroma_siting in [ChromaSiting::Left, ChromaSiting::Center] {
                    let settings = RgbConvertSettings {
                        chroma_filter,
                        chroma_siting,
                        ..settings((64, 64), chroma, BitDepth::Ten, PlaneLayout::Planar)
                    };

                    let kernels = PhaseKernels::build(&settings, gpu::POLAR_CHROMA_WORKGROUP);

                    assert!(
                        kernels.tile_size.0 <= gpu::POLAR_CHROMA_TILE
                            && kernels.tile_size.1 <= gpu::POLAR_CHROMA_TILE,
                        "{chroma_filter} {chroma:?} {chroma_siting:?}: {:?}",
                        kernels.tile_size
                    );
                }
            }
        }
    }

    #[test]
    fn phase_kernels_are_normalized_and_cover_the_anchor() {
        let settings = RgbConvertSettings {
            chroma_filter: ScaleFilter::EwaLanczos,
            ..settings(
                (64, 64),
                ChromaSubsampling::Yuv420,
                BitDepth::Ten,
                PlaneLayout::Planar,
            )
        };

        let kernels = PhaseKernels::build(&settings, gpu::POLAR_CHROMA_WORKGROUP);
        let taps = kernels.taps_per_phase as usize;

        assert_eq!(kernels.phase_rows, 2);
        assert_eq!(kernels.weights.len(), 4 * taps);
        for phase in 0..4 {
            let weights = &kernels.weights[phase * taps..(phase + 1) * taps];
            let offsets = &kernels.offsets[2 * phase * taps..2 * (phase + 1) * taps];

            assert!((weights.iter().sum::<f32>() - 1.0).abs() < 1e-5);
            assert!(offsets.as_chunks::<2>().0.contains(&[0, 0]));
        }
    }

    #[test]
    fn chroma_antiringing_matches_reference() {
        for chroma_filter in [ScaleFilter::Lanczos4, ScaleFilter::EwaLanczosSharp] {
            for chroma in [ChromaSubsampling::Yuv420, ChromaSubsampling::Yuv422] {
                for chroma_antiringing in [0.5, 1.0] {
                    assert_matches_reference(RgbConvertSettings {
                        chroma_filter,
                        chroma_antiringing,
                        ..settings((41, 27), chroma, BitDepth::Ten, PlaneLayout::SemiPlanar)
                    });
                }
            }
        }
    }

    #[test]
    fn full_chroma_antiringing_removes_chroma_overshoot() {
        let Some(gpu) = testing::gpu() else {
            return;
        };
        let image = Image::from_fn(48, 24, |x, _| {
            if x < 24 {
                [0.9, 0.05, 0.05, 1.0]
            } else {
                [0.05, 0.05, 0.9, 1.0]
            }
        });

        for chroma_filter in [ScaleFilter::Lanczos4, ScaleFilter::EwaLanczosSharp] {
            let ringing = RgbConvertSettings {
                chroma_filter,
                output: OutputLight::Encoded,
                ..settings(
                    (48, 24),
                    ChromaSubsampling::Yuv420,
                    BitDepth::Twelve,
                    PlaneLayout::Planar,
                )
            };
            let clamped = RgbConvertSettings {
                chroma_antiringing: 1.0,
                ..ringing
            };

            let bytes = encode_on_gpu(gpu, &image, &ringing);
            let overshoot = chroma_overshoot(&reference::unpack_rgb(&bytes, &ringing), &image);
            let antiringed = chroma_overshoot(&unpack_on_gpu(gpu, &bytes, clamped), &image);

            assert!(overshoot > 0.01, "{chroma_filter}: overshoot {overshoot}");
            assert!(
                antiringed < overshoot / 4.0,
                "{chroma_filter}: antiringed {antiringed} against {overshoot}"
            );
        }
    }

    fn chroma_overshoot(actual: &Image, source: &Image) -> f32 {
        let blue_floor = source.texel(0, 0)[2];
        let red_floor = source.texel(i64::from(source.width) - 1, 0)[0];

        (0..i64::from(actual.width))
            .map(|x| {
                let texel = actual.texel(x, i64::from(actual.height) / 2);
                (blue_floor - texel[2]).max(red_floor - texel[0]).max(0.0)
            })
            .fold(0.0, f32::max)
    }

    #[test]
    fn host_conversion_round_trips_to_the_source_image() {
        let Some(gpu) = testing::gpu() else {
            return;
        };
        let image = Image::from_fn(48, 32, |x, y| {
            let u = f32::from(u16::try_from(x).unwrap()) / 47.0;
            let v = f32::from(u16::try_from(y).unwrap()) / 31.0;
            [0.2 + 0.6 * u, 0.2 + 0.6 * v, 0.8 - 0.3 * (u + v), 1.0]
        });

        for (chroma, chroma_filter) in [
            (ChromaSubsampling::Yuv444, ScaleFilter::Lanczos3),
            (ChromaSubsampling::Yuv420, ScaleFilter::Lanczos3),
            (ChromaSubsampling::Yuv420, ScaleFilter::EwaLanczosSharp),
            (ChromaSubsampling::Yuv422, ScaleFilter::EwaRobidoux),
        ] {
            let settings = RgbConvertSettings {
                chroma_filter,
                ..settings((48, 32), chroma, BitDepth::Ten, PlaneLayout::Planar)
            };

            let bytes = encode_on_gpu(gpu, &image, &settings);
            let unpacked = unpack_on_gpu(gpu, &bytes, settings);
            let edges = unpacked.max_difference(&image);
            let interior = interior_difference(&unpacked, &image, ROUND_TRIP_BORDER);

            assert!(
                edges <= ROUND_TRIP_EDGE_TOLERANCE,
                "{chroma:?} {chroma_filter}: max difference {edges}"
            );
            assert!(
                interior <= ROUND_TRIP_INTERIOR_TOLERANCE,
                "{chroma:?} {chroma_filter}: interior difference {interior}"
            );
        }
    }

    #[test]
    fn inverse_matrix_undoes_the_forward_matrix() {
        for matrix in MatrixCoefficients::ALL {
            let forward = matrix.rows();
            let inverse = matrix.inverse_rows();

            for (row, inverse_row) in inverse.iter().enumerate() {
                for (column, _) in forward.iter().enumerate() {
                    let product: f64 = (0..3)
                        .map(|index| inverse_row[index] * forward[index][column])
                        .sum();
                    let identity = if row == column { 1.0 } else { 0.0 };

                    assert!(
                        (product - identity).abs() < 1e-12,
                        "{matrix:?} {row} {column}"
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let base = settings(
            (16, 16),
            ChromaSubsampling::Yuv420,
            BitDepth::Eight,
            PlaneLayout::Planar,
        );

        assert_eq!(
            RgbConvertSettings {
                chroma_antiringing: 1.25,
                ..base
            }
            .validate(),
            Err(ScaleError::AntiringingOutOfRange(1.25))
        );
        assert_eq!(
            RgbConvertSettings {
                chroma_filter: ScaleFilter::EwaLanczosSharp,
                ..base
            }
            .validate(),
            Ok(())
        );
        assert_eq!(
            RgbConvertSettings {
                transfer: TransferFunction::Hlg,
                ..base
            }
            .validate(),
            Err(ScaleError::LinearOutputTransferUnsupported(
                TransferFunction::Hlg
            ))
        );
        assert_eq!(
            RgbConvertSettings {
                transfer: TransferFunction::Hlg,
                output: OutputLight::Encoded,
                ..base
            }
            .validate(),
            Ok(())
        );
        assert_eq!(
            RgbConvertSettings {
                chroma_antiringing: 0.0,
                row_alignment: 6,
                ..base
            }
            .frame_layout(),
            Err(ScaleError::RowAlignmentInvalid(6))
        );
    }

    #[test]
    fn frames_of_another_layout_are_refused() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);
        let eight = settings(
            (16, 16),
            ChromaSubsampling::Yuv420,
            BitDepth::Eight,
            PlaneLayout::Planar,
        );
        let ten = RgbConvertSettings {
            format: PixelFormat {
                chroma: ChromaSubsampling::Yuv420,
                depth: BitDepth::Ten,
            },
            ..eight
        };
        let mut converter = RgbConverter::new(&pipelines, &gpu.device, eight).unwrap();
        let stale = converter.create_frame(&gpu.device);
        let target = testing::storage_target(gpu, 16, 16);

        let reconfigured = converter.configure(&gpu.device, ten).unwrap();
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let result = converter.encode(&gpu.device, &mut encoder, &stale, &testing::view(&target));

        assert_eq!(reconfigured, Reconfigured::Tables);
        assert_eq!(result, Err(ScaleError::FrameLayoutMismatch));
    }
}
