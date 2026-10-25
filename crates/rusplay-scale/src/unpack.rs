use rusplay_core::{
    ChromaSubsampling, ColorRange, MatrixCoefficients, PixelFormat, Resolution, ScaleFilter,
    TransferFunction,
};

use crate::{
    ChromaSiting, DmaBufTexture, FrameLayout, Geometry, Kernel, PlaneContent, PlaneLayout,
    Quantization, Reconfigured, ScaleError, TapTable, YuvFrame,
    convert::chroma_size,
    gpu::{
        self, CHROMA_FORMAT, ScalePipelines, ShaderPass, buffer_group, storage_buffer,
        uniform_buffer,
    },
    scaler::{GroupCache, resolution_pair},
};

const WORD_BYTES: u64 = 4;
const LEFT_SITING_UPSAMPLE_PHASE: f64 = 0.25;
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
    pub row_alignment: u32,
}

impl RgbConvertSettings {
    fn validate(&self) -> Result<(), ScaleError> {
        if self.resolution.pixel_count() == 0 {
            return Err(ScaleError::EmptySource(self.resolution));
        }
        if Kernel::new(self.chroma_filter).geometry() != Geometry::Separable {
            return Err(ScaleError::UpsampleChromaFilterNotSeparable(
                self.chroma_filter,
            ));
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
        )
    }

    pub fn chroma_size(&self) -> (u32, u32) {
        chroma_size(self.resolution, self.format.chroma)
    }

    pub(crate) fn chroma_tables(&self) -> (TapTable, TapTable) {
        let kernel = Kernel::new(self.chroma_filter);
        let (width, height) = resolution_pair(self.resolution);
        let (chroma_width, chroma_height) = self.chroma_size();
        let phase = match self.chroma_siting {
            ChromaSiting::Left => LEFT_SITING_UPSAMPLE_PHASE,
            ChromaSiting::Center => 0.0,
        };

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
        })
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
        Ok(Reconfigured::Tables)
    }

    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        frame: &YuvFrame,
        target: &wgpu::TextureView,
    ) -> Result<(), ScaleError> {
        if *frame.layout() != self.layout {
            return Err(ScaleError::FrameLayoutMismatch);
        }

        let pipelines = &self.pipelines;
        let input_group = self.inputs.get_or_create(frame.buffer(), || {
            buffer_group(
                device,
                "rusplay-unpack-input",
                &pipelines.unpack.input_layout,
                &[frame.buffer()],
            )
        });
        let target_group = self
            .targets
            .get_or_create(target, || pipelines.destination_group(device, target));

        self.plan.encode(
            [&pipelines.unpack.chroma, &pipelines.unpack.rgb],
            encoder,
            &input_group,
            &target_group,
        );
        Ok(())
    }

    pub fn encode_planes(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &DmaBufTexture,
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
        let (Some(luma), Some(chroma)) = (source.view(0), source.view(1)) else {
            return Err(mismatch);
        };

        let found = source.resolution();
        let needed = self.settings.resolution;
        if found.width < needed.width || found.height < needed.height {
            return Err(ScaleError::DmaBufTooSmall { found, needed });
        }

        let pipelines = &self.pipelines;
        let input_group = self
            .planes
            .get_or_create(&(luma.clone(), chroma.clone()), || {
                planes_group(device, &pipelines.unpack.planes_layout, luma, chroma)
            });
        let target_group = self
            .targets
            .get_or_create(target, || pipelines.destination_group(device, target));

        self.plan.encode(
            [
                &pipelines.unpack.planes_chroma,
                &pipelines.unpack.planes_rgb,
            ],
            encoder,
            &input_group,
            &target_group,
        );
        Ok(())
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
    rgb_pass_size: (u32, u32),
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
        let chroma_height = settings.chroma_size().1;

        let params = uniform_buffer(
            device,
            "rusplay-unpack-params",
            &params_bytes(settings, layout, &horizontal, &vertical)?,
        );
        let horizontal_firsts = storage_buffer(device, "rusplay-unpack-firsts", &horizontal.firsts);
        let horizontal_weights =
            storage_buffer(device, "rusplay-unpack-weights", &horizontal.weights);
        let vertical_firsts = storage_buffer(device, "rusplay-unpack-firsts", &vertical.firsts);
        let vertical_weights = storage_buffer(device, "rusplay-unpack-weights", &vertical.weights);
        let intermediate = gpu::storage_texture(
            device,
            "rusplay-unpack-chroma",
            (size.0, chroma_height),
            CHROMA_FORMAT,
        );

        Ok(Self {
            chroma_group: buffer_group(
                device,
                "rusplay-unpack-chroma",
                &pipelines.convert.table_layout,
                &[&params, &horizontal_firsts, &horizontal_weights],
            ),
            rgb_group: buffer_group(
                device,
                "rusplay-unpack-rgb",
                &pipelines.convert.table_layout,
                &[&params, &vertical_firsts, &vertical_weights],
            ),
            chroma_target: gpu::view_group(
                device,
                &pipelines.convert.chroma_target_layout,
                &intermediate,
            ),
            chroma_source: pipelines.source_group(device, &intermediate),
            chroma_pass_size: (size.0, chroma_height),
            rgb_pass_size: size,
        })
    }

    fn encode(
        &self,
        passes: [&ShaderPass; 2],
        encoder: &mut wgpu::CommandEncoder,
        input_group: &wgpu::BindGroup,
        target_group: &wgpu::BindGroup,
    ) {
        let [chroma_pass, rgb_pass] = passes;

        gpu::dispatch(
            encoder,
            "rusplay-unpack-chroma",
            chroma_pass,
            &[input_group, &self.chroma_target, &self.chroma_group],
            self.chroma_pass_size,
        );
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
    horizontal: &TapTable,
    vertical: &TapTable,
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
        horizontal.taps_per_pixel,
        vertical.taps_per_pixel,
    ];

    Ok(words.iter().flat_map(|word| word.to_ne_bytes()).collect())
}

#[cfg(test)]
mod tests {
    use rusplay_core::BitDepth;

    use super::*;
    use crate::{
        ColorConverter, ConvertSettings, InputLight, reference,
        testing::{self, Image, RGBA16F_TOLERANCE},
    };

    const ROUND_TRIP_EDGE_TOLERANCE: f32 = 0.03;
    const ROUND_TRIP_INTERIOR_TOLERANCE: f32 = 0.006;
    const ROUND_TRIP_BORDER: i64 = 4;

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
        let Some(gpu) = testing::gpu() else {
            return;
        };
        let image = Image::pattern(settings.resolution.width, settings.resolution.height, 7);

        let bytes = encode_on_gpu(gpu, &image, &settings);
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
    fn host_conversion_round_trips_to_the_source_image() {
        let Some(gpu) = testing::gpu() else {
            return;
        };
        let image = Image::from_fn(48, 32, |x, y| {
            let u = f32::from(u16::try_from(x).unwrap()) / 47.0;
            let v = f32::from(u16::try_from(y).unwrap()) / 31.0;
            [0.2 + 0.6 * u, 0.2 + 0.6 * v, 0.8 - 0.3 * (u + v), 1.0]
        });

        for chroma in [ChromaSubsampling::Yuv444, ChromaSubsampling::Yuv420] {
            let settings = settings((48, 32), chroma, BitDepth::Ten, PlaneLayout::Planar);

            let bytes = encode_on_gpu(gpu, &image, &settings);
            let unpacked = unpack_on_gpu(gpu, &bytes, settings);
            let edges = unpacked.max_difference(&image);
            let interior = interior_difference(&unpacked, &image, ROUND_TRIP_BORDER);

            assert!(
                edges <= ROUND_TRIP_EDGE_TOLERANCE,
                "{chroma:?}: max difference {edges}"
            );
            assert!(
                interior <= ROUND_TRIP_INTERIOR_TOLERANCE,
                "{chroma:?}: interior difference {interior}"
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
                chroma_filter: ScaleFilter::EwaLanczos,
                ..base
            }
            .validate(),
            Err(ScaleError::UpsampleChromaFilterNotSeparable(
                ScaleFilter::EwaLanczos
            ))
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
