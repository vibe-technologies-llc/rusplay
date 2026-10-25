use rusplay_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, HdrLevels, PixelFormat, Resolution,
    ScaleFilter, TransferFunction,
};

use crate::{
    Geometry, Kernel, Reconfigured, ScaleError, TapTable,
    gpu::{
        self, CHROMA_FORMAT, ScalePipelines, buffer_group, storage_buffer, uniform_buffer,
        view_group,
    },
    polar::{PolarGeometry, PolarPlan},
    scaler::{GroupCache, resolution_pair},
};

pub const DEFAULT_ROW_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

const MIN_ROW_ALIGNMENT: u32 = 4;
const WORD_BYTES: u64 = 4;
const LEFT_SITING_PHASE: f64 = -0.5;
const SUBSAMPLING_RATIO: f32 = 2.0;
const TRANSFER_NONE: u32 = 0;
const TRANSFER_SRGB: u32 = 1;
const TRANSFER_BT709: u32 = 2;
const TRANSFER_PQ: u32 = 3;
const TRANSFER_HLG: u32 = 4;
const HLG_REFERENCE_PEAK_NITS: f64 = 1000.0;
const HLG_REFERENCE_GAMMA: f64 = 1.2;
const HLG_GAMMA_PER_DECADE: f64 = 0.42;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum YuvMatrix {
    Bt709,
    Bt2020,
}

impl YuvMatrix {
    pub const fn for_primaries(primaries: ColorPrimaries) -> Self {
        match primaries {
            ColorPrimaries::Bt709 => Self::Bt709,
            ColorPrimaries::Bt2020 => Self::Bt2020,
        }
    }

    pub const fn red_blue_coefficients(self) -> (f64, f64) {
        match self {
            Self::Bt709 => (0.2126, 0.0722),
            Self::Bt2020 => (0.2627, 0.0593),
        }
    }

    pub fn rows(self) -> [[f64; 3]; 3] {
        let (red, blue) = self.red_blue_coefficients();
        let green = 1.0 - red - blue;
        let blue_scale = 2.0 * (1.0 - blue);
        let red_scale = 2.0 * (1.0 - red);

        [
            [red, green, blue],
            [-red / blue_scale, -green / blue_scale, 0.5],
            [0.5, -green / red_scale, -blue / red_scale],
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaneLayout {
    Planar,
    SemiPlanar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChromaSiting {
    #[default]
    Left,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputLight {
    Linear,
    Encoded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleAlignment {
    Low,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaneContent {
    Luma,
    Cb,
    Cr,
    CbCr,
}

impl PlaneContent {
    pub const fn samples_per_pixel(self) -> u32 {
        match self {
            Self::Luma | Self::Cb | Self::Cr => 1,
            Self::CbCr => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConvertSettings {
    pub resolution: Resolution,
    pub format: PixelFormat,
    pub layout: PlaneLayout,
    pub matrix: YuvMatrix,
    pub range: ColorRange,
    pub transfer: TransferFunction,
    pub hdr: HdrLevels,
    pub input: InputLight,
    pub chroma_filter: ScaleFilter,
    pub chroma_siting: ChromaSiting,
    pub row_alignment: u32,
}

impl ConvertSettings {
    fn validate(&self) -> Result<(), ScaleError> {
        if self.resolution.pixel_count() == 0 {
            return Err(ScaleError::EmptySource(self.resolution));
        }
        if self.polar_chroma() && self.format.chroma == ChromaSubsampling::Yuv422 {
            return Err(ScaleError::PolarChromaFilterUnsupported {
                filter: self.chroma_filter,
                chroma: self.format.chroma,
            });
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

    fn transfer_code(&self) -> u32 {
        match (self.input, self.transfer) {
            (InputLight::Encoded, _) => TRANSFER_NONE,
            (InputLight::Linear, TransferFunction::Srgb) => TRANSFER_SRGB,
            (InputLight::Linear, TransferFunction::Bt709) => TRANSFER_BT709,
            (InputLight::Linear, TransferFunction::Pq) => TRANSFER_PQ,
            (InputLight::Linear, TransferFunction::Hlg) => TRANSFER_HLG,
        }
    }

    pub(crate) fn hdr_scale(&self) -> f64 {
        let white = f64::from(self.hdr.reference_white_nits);
        match self.transfer {
            TransferFunction::Hlg => white / f64::from(self.hdr.peak_nits),
            _ => white / f64::from(HdrLevels::MAX_NITS),
        }
    }

    pub(crate) fn hlg_exponent(&self) -> f64 {
        let peak = f64::from(self.hdr.peak_nits);
        let gamma =
            HLG_REFERENCE_GAMMA + HLG_GAMMA_PER_DECADE * (peak / HLG_REFERENCE_PEAK_NITS).log10();

        (1.0 - gamma) / gamma
    }

    pub(crate) fn polar_chroma(&self) -> bool {
        Kernel::new(self.chroma_filter).geometry() == Geometry::Polar
            && self.format.chroma != ChromaSubsampling::Yuv444
    }

    fn siting_phase(&self) -> f64 {
        match self.chroma_siting {
            ChromaSiting::Left => LEFT_SITING_PHASE,
            ChromaSiting::Center => 0.0,
        }
    }

    pub(crate) fn polar_chroma_geometry(&self) -> PolarGeometry {
        #[allow(clippy::cast_possible_truncation)]
        let phase = self.siting_phase() as f32;

        PolarGeometry::with_ratio(
            self.chroma_filter,
            resolution_pair(self.resolution),
            self.chroma_size(),
            (SUBSAMPLING_RATIO, SUBSAMPLING_RATIO),
            (phase, 0.0),
        )
    }

    pub fn chroma_size(&self) -> (u32, u32) {
        let (width, height) = resolution_pair(self.resolution);

        match self.format.chroma {
            ChromaSubsampling::Yuv420 => (width.div_ceil(2), height.div_ceil(2)),
            ChromaSubsampling::Yuv422 => (width.div_ceil(2), height),
            ChromaSubsampling::Yuv444 => (width, height),
        }
    }

    pub fn quantization(&self) -> Quantization {
        let bits = self.format.depth.bits();
        let max_code = (1_u32 << bits) - 1;
        let step = f64::from(1_u32 << (bits - 8));

        let (luma, chroma) = match self.range {
            ColorRange::Full => (
                (f64::from(max_code), 0.0),
                (f64::from(max_code), f64::from(1_u32 << (bits - 1))),
            ),
            ColorRange::Limited => ((219.0 * step, 16.0 * step), (224.0 * step, 128.0 * step)),
        };

        Quantization {
            luma,
            chroma,
            max_code,
        }
    }

    pub(crate) fn chroma_tables(&self) -> (TapTable, TapTable) {
        let kernel = Kernel::new(self.chroma_filter);
        let (width, height) = resolution_pair(self.resolution);
        let (chroma_width, chroma_height) = self.chroma_size();
        let phase = self.siting_phase();

        if self.polar_chroma() {
            return (
                TapTable::identity(chroma_width),
                TapTable::identity(chroma_height),
            );
        }

        let horizontal = match self.format.chroma {
            ChromaSubsampling::Yuv444 => TapTable::identity(width),
            ChromaSubsampling::Yuv420 | ChromaSubsampling::Yuv422 => {
                TapTable::build_with_phase(kernel, 2 * chroma_width, chroma_width, phase)
            }
        };
        let vertical = match self.format.chroma {
            ChromaSubsampling::Yuv420 => TapTable::build(kernel, 2 * chroma_height, chroma_height),
            ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444 => TapTable::identity(height),
        };

        (horizontal, vertical)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantization {
    pub luma: (f64, f64),
    pub chroma: (f64, f64),
    pub max_code: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Plane {
    pub content: PlaneContent,
    pub offset: u64,
    pub stride: u32,
    pub width: u32,
    pub height: u32,
}

impl Plane {
    pub fn size(&self) -> u64 {
        u64::from(self.stride) * u64::from(self.height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrameLayout {
    pub planes: Vec<Plane>,
    pub size: u64,
    pub depth: BitDepth,
    pub bytes_per_sample: u32,
    pub alignment: SampleAlignment,
}

impl FrameLayout {
    pub fn new(settings: &ConvertSettings) -> Result<Self, ScaleError> {
        let alignment = settings.row_alignment;
        if alignment < MIN_ROW_ALIGNMENT || !alignment.is_power_of_two() {
            return Err(ScaleError::RowAlignmentInvalid(alignment));
        }

        let depth = settings.format.depth;
        let bytes_per_sample = if depth.bits() <= 8 { 1 } else { 2 };
        let (width, height) = resolution_pair(settings.resolution);
        let (chroma_width, chroma_height) = settings.chroma_size();

        let shapes: &[(PlaneContent, u32, u32)] = match settings.layout {
            PlaneLayout::Planar => &[
                (PlaneContent::Luma, width, height),
                (PlaneContent::Cb, chroma_width, chroma_height),
                (PlaneContent::Cr, chroma_width, chroma_height),
            ],
            PlaneLayout::SemiPlanar => &[
                (PlaneContent::Luma, width, height),
                (PlaneContent::CbCr, chroma_width, chroma_height),
            ],
        };

        let mut planes = Vec::with_capacity(shapes.len());
        let mut size = 0_u64;
        for &(content, plane_width, plane_height) in shapes {
            let row_bytes = u64::from(plane_width)
                * u64::from(content.samples_per_pixel())
                * u64::from(bytes_per_sample);
            let stride = u32::try_from(row_bytes.next_multiple_of(u64::from(alignment)))
                .map_err(|_| ScaleError::FrameTooLarge { bytes: row_bytes })?;
            let plane = Plane {
                content,
                offset: size,
                stride,
                width: plane_width,
                height: plane_height,
            };
            size += plane.size();
            planes.push(plane);
        }

        if size / WORD_BYTES > u64::from(u32::MAX) {
            return Err(ScaleError::FrameTooLarge { bytes: size });
        }

        Ok(Self {
            planes,
            size,
            depth,
            bytes_per_sample,
            alignment: match (settings.layout, bytes_per_sample) {
                (PlaneLayout::SemiPlanar, 2) => SampleAlignment::High,
                _ => SampleAlignment::Low,
            },
        })
    }

    pub fn sample_shift(&self) -> u32 {
        match self.alignment {
            SampleAlignment::Low => 0,
            SampleAlignment::High => 8 * self.bytes_per_sample - u32::from(self.depth.bits()),
        }
    }

    pub fn plane(&self, content: PlaneContent) -> Option<&Plane> {
        self.planes.iter().find(|plane| plane.content == content)
    }
}

#[derive(Debug, Clone)]
pub struct YuvFrame {
    buffer: wgpu::Buffer,
    layout: FrameLayout,
}

impl YuvFrame {
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub fn layout(&self) -> &FrameLayout {
        &self.layout
    }
}

#[derive(Debug)]
pub struct ColorConverter {
    pipelines: ScalePipelines,
    settings: ConvertSettings,
    layout: FrameLayout,
    plan: ConvertPlan,
    sources: GroupCache<wgpu::TextureView>,
    outputs: GroupCache<wgpu::Buffer>,
}

impl ColorConverter {
    pub fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: ConvertSettings,
    ) -> Result<Self, ScaleError> {
        settings.validate()?;
        let layout = FrameLayout::new(&settings)?;

        Ok(Self {
            pipelines: pipelines.clone(),
            plan: ConvertPlan::new(pipelines, device, &settings, &layout)?,
            settings,
            layout,
            sources: GroupCache::default(),
            outputs: GroupCache::default(),
        })
    }

    pub fn settings(&self) -> ConvertSettings {
        self.settings
    }

    pub fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    pub fn create_frame(&self, device: &wgpu::Device) -> YuvFrame {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rusplay-yuv-frame"),
            size: self.layout.size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        YuvFrame {
            buffer,
            layout: self.layout.clone(),
        }
    }

    pub fn configure(
        &mut self,
        device: &wgpu::Device,
        settings: ConvertSettings,
    ) -> Result<Reconfigured, ScaleError> {
        if settings == self.settings {
            return Ok(Reconfigured::Unchanged);
        }
        settings.validate()?;
        let layout = FrameLayout::new(&settings)?;

        self.plan = ConvertPlan::new(&self.pipelines, device, &settings, &layout)?;
        self.settings = settings;
        self.layout = layout;
        Ok(Reconfigured::Tables)
    }

    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        frame: &YuvFrame,
    ) -> Result<(), ScaleError> {
        if frame.layout != self.layout {
            return Err(ScaleError::FrameLayoutMismatch);
        }

        let pipelines = &self.pipelines;
        let source_group = self
            .sources
            .get_or_create(source, || pipelines.source_group(device, source));
        let output_group = self.outputs.get_or_create(&frame.buffer, || {
            buffer_group(
                device,
                "rusplay-convert-output",
                &pipelines.convert.output_layout,
                &[&frame.buffer],
            )
        });

        self.plan
            .encode(pipelines, encoder, &source_group, &output_group);
        Ok(())
    }
}

#[derive(Debug)]
struct ConvertPlan {
    luma_words: (u32, u32),
    path: ChromaPath,
}

#[derive(Debug)]
enum ChromaPath {
    Direct {
        group: wgpu::BindGroup,
    },
    Rows {
        rows_group: wgpu::BindGroup,
        pack: PackStage,
    },
    Polar {
        full_group: wgpu::BindGroup,
        full_target: wgpu::BindGroup,
        full_source: wgpu::BindGroup,
        resample: PolarPlan,
        pack: PackStage,
    },
}

#[derive(Debug)]
struct PackStage {
    target: wgpu::BindGroup,
    source: wgpu::BindGroup,
    group: wgpu::BindGroup,
    words: (u32, u32),
    layers: u32,
}

impl PackStage {
    fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        layout: &FrameLayout,
        params: &wgpu::Buffer,
        vertical: &TapTable,
        intermediate_size: (u32, u32),
    ) -> Self {
        let chroma = layout.planes[1];
        let last = layout.planes[layout.planes.len() - 1];
        let intermediate = gpu::storage_texture(
            device,
            "rusplay-convert-chroma",
            intermediate_size,
            CHROMA_FORMAT,
        );
        let firsts = storage_buffer(device, "rusplay-convert-firsts", &vertical.firsts);
        let weights = storage_buffer(device, "rusplay-convert-weights", &vertical.weights);

        Self {
            target: view_group(
                device,
                &pipelines.convert.chroma_target_layout,
                &intermediate,
            ),
            source: pipelines.source_group(device, &intermediate),
            group: buffer_group(
                device,
                "rusplay-pack-chroma",
                &pipelines.convert.table_layout,
                &[params, &firsts, &weights],
            ),
            words: (chroma.stride / 4, chroma.height),
            layers: if last.content == PlaneContent::Cr {
                2
            } else {
                1
            },
        }
    }

    fn encode(
        &self,
        pipelines: &ScalePipelines,
        encoder: &mut wgpu::CommandEncoder,
        output_group: &wgpu::BindGroup,
    ) {
        gpu::dispatch_layers(
            encoder,
            "rusplay-pack-chroma",
            &pipelines.convert.pack_chroma,
            &[&self.source, output_group, &self.group],
            self.words,
            self.layers,
        );
    }
}

impl ConvertPlan {
    fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: &ConvertSettings,
        layout: &FrameLayout,
    ) -> Result<Self, ScaleError> {
        let (horizontal, vertical) = settings.chroma_tables();
        let luma = layout.planes[0];
        let params = uniform_buffer(
            device,
            "rusplay-convert-params",
            &params_bytes(settings, layout, &horizontal, &vertical)?,
        );

        let path = if settings.format.chroma == ChromaSubsampling::Yuv444 {
            ChromaPath::Direct {
                group: buffer_group(
                    device,
                    "rusplay-convert-direct",
                    &pipelines.convert.params_layout,
                    &[&params],
                ),
            }
        } else if settings.polar_chroma() {
            Self::polar_path(pipelines, device, settings, layout, &params, &vertical)
        } else {
            Self::rows_path(
                pipelines,
                device,
                settings,
                layout,
                &params,
                (&horizontal, &vertical),
            )
        };

        Ok(Self {
            luma_words: (luma.stride / 4, luma.height),
            path,
        })
    }

    fn rows_path(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: &ConvertSettings,
        layout: &FrameLayout,
        params: &wgpu::Buffer,
        (horizontal, vertical): (&TapTable, &TapTable),
    ) -> ChromaPath {
        let chroma_width = settings.chroma_size().0;
        let firsts = storage_buffer(device, "rusplay-convert-firsts", &horizontal.firsts);
        let weights = storage_buffer(device, "rusplay-convert-weights", &horizontal.weights);

        ChromaPath::Rows {
            rows_group: buffer_group(
                device,
                "rusplay-convert-rows",
                &pipelines.convert.table_layout,
                &[params, &firsts, &weights],
            ),
            pack: PackStage::new(
                pipelines,
                device,
                layout,
                params,
                vertical,
                (chroma_width, settings.resolution.height),
            ),
        }
    }

    fn polar_path(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: &ConvertSettings,
        layout: &FrameLayout,
        params: &wgpu::Buffer,
        vertical: &TapTable,
    ) -> ChromaPath {
        let full = gpu::storage_texture(
            device,
            "rusplay-convert-full-chroma",
            resolution_pair(settings.resolution),
            CHROMA_FORMAT,
        );

        ChromaPath::Polar {
            full_group: buffer_group(
                device,
                "rusplay-convert-full",
                &pipelines.convert.params_layout,
                &[params],
            ),
            full_target: view_group(device, &pipelines.convert.chroma_target_layout, &full),
            full_source: pipelines.source_group(device, &full),
            resample: PolarPlan::chroma(
                pipelines,
                device,
                settings.chroma_filter,
                settings.polar_chroma_geometry(),
            ),
            pack: PackStage::new(
                pipelines,
                device,
                layout,
                params,
                vertical,
                settings.chroma_size(),
            ),
        }
    }

    fn encode(
        &self,
        pipelines: &ScalePipelines,
        encoder: &mut wgpu::CommandEncoder,
        source_group: &wgpu::BindGroup,
        output_group: &wgpu::BindGroup,
    ) {
        match &self.path {
            ChromaPath::Direct { group } => gpu::dispatch(
                encoder,
                "rusplay-convert-direct",
                &pipelines.convert.direct,
                &[source_group, output_group, group],
                self.luma_words,
            ),
            ChromaPath::Rows { rows_group, pack } => {
                gpu::dispatch(
                    encoder,
                    "rusplay-convert-rows",
                    &pipelines.convert.rows,
                    &[source_group, output_group, rows_group, &pack.target],
                    self.luma_words,
                );
                pack.encode(pipelines, encoder, output_group);
            }
            ChromaPath::Polar {
                full_group,
                full_target,
                full_source,
                resample,
                pack,
            } => {
                gpu::dispatch(
                    encoder,
                    "rusplay-convert-full",
                    &pipelines.convert.full,
                    &[source_group, output_group, full_group, full_target],
                    self.luma_words,
                );
                resample.encode(encoder, full_source, &pack.target);
                pack.encode(pipelines, encoder, output_group);
            }
        }
    }
}

fn params_bytes(
    settings: &ConvertSettings,
    layout: &FrameLayout,
    horizontal: &TapTable,
    vertical: &TapTable,
) -> Result<Vec<u8>, ScaleError> {
    let rows = settings.matrix.rows();
    let quantization = settings.quantization();
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
        settings.transfer_code(),
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
        float(settings.hdr_scale()),
        float(settings.hlg_exponent()),
        0,
        0,
    ];

    Ok(words.iter().flat_map(|word| word.to_ne_bytes()).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gpu::MAX_CHROMA_TAPS,
        reference::{self, SamplePlanes},
        testing::{self, Image},
    };

    const CODE_TOLERANCE: u32 = 1;

    fn settings(
        resolution: (u32, u32),
        chroma: ChromaSubsampling,
        depth: BitDepth,
        layout: PlaneLayout,
    ) -> ConvertSettings {
        ConvertSettings {
            resolution: Resolution::new(resolution.0, resolution.1),
            format: PixelFormat { chroma, depth },
            layout,
            matrix: YuvMatrix::Bt709,
            range: ColorRange::Limited,
            transfer: TransferFunction::Srgb,
            hdr: HdrLevels::default(),
            input: InputLight::Linear,
            chroma_filter: ScaleFilter::Hermite,
            chroma_siting: ChromaSiting::Left,
            row_alignment: DEFAULT_ROW_ALIGNMENT,
        }
    }

    fn convert_on_gpu(image: &Image, settings: ConvertSettings) -> Option<SamplePlanes> {
        let gpu = testing::gpu()?;

        let pipelines = ScalePipelines::new(&gpu.device);
        let mut converter = ColorConverter::new(&pipelines, &gpu.device, settings).unwrap();
        let frame = converter.create_frame(&gpu.device);
        let source = testing::upload(gpu, image);

        testing::submit(gpu, |encoder| {
            converter
                .encode(&gpu.device, encoder, &testing::view(&source), &frame)
                .unwrap();
        });
        let bytes = testing::download_buffer(gpu, frame.buffer());

        Some(reference::unpack(&bytes, frame.layout()))
    }

    fn max_code_difference(actual: &SamplePlanes, expected: &SamplePlanes) -> u32 {
        assert_eq!(actual.len(), expected.len());

        actual
            .iter()
            .zip(expected)
            .flat_map(|(actual_plane, expected_plane)| {
                assert_eq!(actual_plane.len(), expected_plane.len());
                actual_plane.iter().zip(expected_plane)
            })
            .flat_map(|(actual_row, expected_row)| {
                assert_eq!(actual_row.len(), expected_row.len());
                actual_row.iter().zip(expected_row)
            })
            .map(|(actual, expected)| actual.abs_diff(*expected))
            .max()
            .unwrap_or(0)
    }

    fn assert_matches_reference(settings: ConvertSettings) {
        let image = Image::pattern(settings.resolution.width, settings.resolution.height, 21);

        assert_image_matches_reference(&image, settings);
    }

    fn assert_image_matches_reference(image: &Image, settings: ConvertSettings) {
        let Some(actual) = convert_on_gpu(image, settings) else {
            return;
        };
        let difference = max_code_difference(&actual, &reference::convert(image, &settings));

        assert!(
            difference <= CODE_TOLERANCE,
            "{settings:?}: max code difference {difference}"
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
            assert_matches_reference(ConvertSettings {
                matrix: if index % 2 == 0 {
                    YuvMatrix::Bt709
                } else {
                    YuvMatrix::Bt2020
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
            PlaneLayout::SemiPlanar,
        );

        for chroma_filter in [
            ScaleFilter::Bilinear,
            ScaleFilter::CatmullRom,
            ScaleFilter::Mitchell,
            ScaleFilter::Lanczos3,
        ] {
            assert_matches_reference(ConvertSettings {
                chroma_filter,
                chroma_siting: ChromaSiting::Center,
                ..base
            });
        }
        assert_matches_reference(ConvertSettings {
            transfer: TransferFunction::Bt709,
            ..base
        });
        assert_matches_reference(ConvertSettings {
            input: InputLight::Encoded,
            ..base
        });
        assert_matches_reference(ConvertSettings {
            row_alignment: 4,
            ..base
        });
    }

    #[test]
    fn every_polar_chroma_filter_matches_reference() {
        let polar_filters = [
            ScaleFilter::EwaLanczos,
            ScaleFilter::EwaLanczosSharp,
            ScaleFilter::EwaGinseng,
            ScaleFilter::EwaHann,
            ScaleFilter::EwaRobidoux,
            ScaleFilter::EwaRobidouxSharp,
        ];

        for (index, chroma_filter) in polar_filters.into_iter().enumerate() {
            assert_matches_reference(ConvertSettings {
                chroma_filter,
                chroma_siting: if index % 2 == 0 {
                    ChromaSiting::Left
                } else {
                    ChromaSiting::Center
                },
                ..settings(
                    (37, 23),
                    ChromaSubsampling::Yuv420,
                    BitDepth::Ten,
                    PlaneLayout::SemiPlanar,
                )
            });
        }
    }

    #[test]
    fn polar_chroma_matches_reference_across_depths_and_layouts() {
        for (depth, layout) in [
            (BitDepth::Eight, PlaneLayout::Planar),
            (BitDepth::Twelve, PlaneLayout::Planar),
            (BitDepth::Twelve, PlaneLayout::SemiPlanar),
        ] {
            assert_matches_reference(ConvertSettings {
                chroma_filter: ScaleFilter::EwaLanczos,
                row_alignment: 4,
                ..settings((91, 30), ChromaSubsampling::Yuv420, depth, layout)
            });
        }
    }

    #[test]
    fn polar_chroma_siting_follows_the_setting() {
        let ramp = Image::from_fn(64, 8, |x, _| {
            [f32::from(u8::try_from(x).unwrap()) / 63.0, 0.25, 0.25, 1.0]
        });
        let base = ConvertSettings {
            input: InputLight::Encoded,
            chroma_filter: ScaleFilter::EwaLanczos,
            ..settings(
                (64, 8),
                ChromaSubsampling::Yuv420,
                BitDepth::Ten,
                PlaneLayout::Planar,
            )
        };
        let full_chroma = ConvertSettings {
            format: PixelFormat {
                chroma: ChromaSubsampling::Yuv444,
                depth: BitDepth::Ten,
            },
            ..base
        };

        let Some(full) = convert_on_gpu(&ramp, full_chroma) else {
            return;
        };

        for (siting, offset) in [(ChromaSiting::Left, 0.0), (ChromaSiting::Center, 0.5)] {
            let subsampled = convert_on_gpu(
                &ramp,
                ConvertSettings {
                    chroma_siting: siting,
                    ..base
                },
            )
            .unwrap();

            for (x, actual) in subsampled[2][1].iter().enumerate().take(28).skip(4) {
                let at = |column: usize| f64::from(full[2][3][column]);
                let expected = at(2 * x) + offset * (at(2 * x + 1) - at(2 * x));
                let actual = f64::from(*actual);

                assert!(
                    (actual - expected).abs() <= 2.0,
                    "{siting:?} x {x}: {actual} against {expected}"
                );
            }
        }
    }

    #[test]
    fn polar_filter_on_full_chroma_is_ignored() {
        let base = settings(
            (29, 11),
            ChromaSubsampling::Yuv444,
            BitDepth::Ten,
            PlaneLayout::Planar,
        );
        let image = Image::pattern(29, 11, 21);

        let polar = convert_on_gpu(
            &image,
            ConvertSettings {
                chroma_filter: ScaleFilter::EwaLanczos,
                ..base
            },
        );
        let separable = convert_on_gpu(&image, base);

        assert_eq!(polar, separable);
    }

    #[test]
    fn wide_and_unaligned_frames_match_reference() {
        let wide = |chroma, depth, layout| ConvertSettings {
            chroma_filter: ScaleFilter::Lanczos3,
            ..settings((700, 5), chroma, depth, layout)
        };

        assert_matches_reference(wide(
            ChromaSubsampling::Yuv420,
            BitDepth::Eight,
            PlaneLayout::SemiPlanar,
        ));
        assert_matches_reference(wide(
            ChromaSubsampling::Yuv422,
            BitDepth::Ten,
            PlaneLayout::Planar,
        ));
        assert_matches_reference(ConvertSettings {
            row_alignment: 64,
            ..settings(
                (70, 3),
                ChromaSubsampling::Yuv444,
                BitDepth::Eight,
                PlaneLayout::SemiPlanar,
            )
        });
        assert_matches_reference(ConvertSettings {
            row_alignment: 4,
            ..settings(
                (301, 3),
                ChromaSubsampling::Yuv444,
                BitDepth::Ten,
                PlaneLayout::SemiPlanar,
            )
        });
    }

    #[test]
    fn every_separable_chroma_filter_fits_the_row_tile() {
        let filters = [
            ScaleFilter::Bilinear,
            ScaleFilter::Hermite,
            ScaleFilter::CatmullRom,
            ScaleFilter::Mitchell,
            ScaleFilter::Lanczos2,
            ScaleFilter::Lanczos3,
            ScaleFilter::Lanczos4,
        ];

        for chroma_filter in filters {
            let (horizontal, _) = ConvertSettings {
                chroma_filter,
                ..settings(
                    (1920, 1080),
                    ChromaSubsampling::Yuv420,
                    BitDepth::Eight,
                    PlaneLayout::SemiPlanar,
                )
            }
            .chroma_tables();

            assert!(
                horizontal.taps_per_pixel <= MAX_CHROMA_TAPS,
                "{chroma_filter}"
            );
        }
    }

    #[test]
    fn pq_and_hlg_match_reference() {
        let bright = |width, height| {
            Image::from_fn(width, height, |x, y| {
                let base = Image::pattern(width, height, 5).texel(i64::from(x), i64::from(y));
                [base[0] * 6.0, base[1] * 3.0, base[2] * 0.2, 1.0]
            })
        };

        for (index, transfer) in [TransferFunction::Pq, TransferFunction::Hlg]
            .into_iter()
            .enumerate()
        {
            for (hdr, depth) in [
                (HdrLevels::default(), BitDepth::Ten),
                (
                    HdrLevels {
                        reference_white_nits: 100,
                        peak_nits: 600,
                    },
                    BitDepth::Twelve,
                ),
                (
                    HdrLevels {
                        reference_white_nits: 400,
                        peak_nits: 4000,
                    },
                    BitDepth::Ten,
                ),
            ] {
                assert_image_matches_reference(
                    &bright(45, 21),
                    ConvertSettings {
                        transfer,
                        hdr,
                        matrix: YuvMatrix::Bt2020,
                        range: if index == 0 {
                            ColorRange::Limited
                        } else {
                            ColorRange::Full
                        },
                        ..settings(
                            (45, 21),
                            ChromaSubsampling::Yuv420,
                            depth,
                            PlaneLayout::SemiPlanar,
                        )
                    },
                );
            }
        }
    }

    #[test]
    fn hdr_curves_hit_textbook_codes() {
        let grays = Image::from_fn(8, 2, |x, _| match x / 2 {
            0 => [1.0; 4],
            1 => [0.0, 0.0, 0.0, 1.0],
            2 => [0.050_68, 0.050_68, 0.050_68, 1.0],
            _ => [0.5; 4],
        });
        let base = ConvertSettings {
            matrix: YuvMatrix::Bt2020,
            ..settings(
                (8, 2),
                ChromaSubsampling::Yuv444,
                BitDepth::Ten,
                PlaneLayout::Planar,
            )
        };
        let pq = |reference_white_nits| ConvertSettings {
            transfer: TransferFunction::Pq,
            hdr: HdrLevels {
                reference_white_nits,
                ..HdrLevels::default()
            },
            ..base
        };
        let hlg = ConvertSettings {
            transfer: TransferFunction::Hlg,
            hdr: HdrLevels {
                reference_white_nits: 1000,
                peak_nits: 1000,
            },
            ..base
        };

        let Some(pq_hundred) = convert_on_gpu(&grays, pq(100)) else {
            return;
        };
        let pq_reference = convert_on_gpu(&grays, pq(203)).unwrap();
        let pq_peak = convert_on_gpu(&grays, pq(10_000)).unwrap();
        let hlg_planes = convert_on_gpu(&grays, hlg).unwrap();

        assert_eq!(pq_hundred[0][0][0], 509);
        assert_eq!(pq_reference[0][0][0], 573);
        assert_eq!(pq_peak[0][0][0], 940);
        assert_eq!(pq_peak[0][0][2], 64);
        assert_eq!(pq_peak[1][0][0], 512);
        assert_eq!(hlg_planes[0][0][0], 940);
        assert_eq!(hlg_planes[0][0][2], 64);
        assert_eq!(hlg_planes[0][0][4], 502);
    }

    #[test]
    fn primaries_convert_to_textbook_codes() {
        let image = Image::from_fn(8, 2, |x, _| match x / 2 {
            0 => [1.0, 1.0, 1.0, 1.0],
            1 => [0.0, 0.0, 0.0, 1.0],
            2 => [1.0, 0.0, 0.0, 1.0],
            _ => [0.0, 0.0, 1.0, 1.0],
        });
        let limited = ConvertSettings {
            input: InputLight::Encoded,
            chroma_filter: ScaleFilter::Bilinear,
            ..settings(
                (8, 2),
                ChromaSubsampling::Yuv444,
                BitDepth::Eight,
                PlaneLayout::Planar,
            )
        };
        let full_ten = ConvertSettings {
            range: ColorRange::Full,
            format: PixelFormat {
                chroma: ChromaSubsampling::Yuv444,
                depth: BitDepth::Ten,
            },
            ..limited
        };

        let Some(limited_planes) = convert_on_gpu(&image, limited) else {
            return;
        };
        let full_planes = convert_on_gpu(&image, full_ten).unwrap();
        let pixel = |planes: &SamplePlanes, x: usize| [0, 1, 2].map(|plane| planes[plane][0][x]);

        assert_eq!(pixel(&limited_planes, 0), [235, 128, 128]);
        assert_eq!(pixel(&limited_planes, 2), [16, 128, 128]);
        assert_eq!(pixel(&limited_planes, 4), [63, 102, 240]);
        assert_eq!(pixel(&limited_planes, 6), [32, 240, 118]);
        assert_eq!(pixel(&full_planes, 0), [1023, 512, 512]);
        assert_eq!(pixel(&full_planes, 2), [0, 512, 512]);
    }

    #[test]
    fn nv12_and_p010_layouts_are_encoder_ready() {
        let nv12 = FrameLayout::new(&ConvertSettings {
            row_alignment: 64,
            ..settings(
                (1920, 1080),
                ChromaSubsampling::Yuv420,
                BitDepth::Eight,
                PlaneLayout::SemiPlanar,
            )
        })
        .unwrap();
        let p010 = FrameLayout::new(&ConvertSettings {
            row_alignment: 64,
            ..settings(
                (1920, 1080),
                ChromaSubsampling::Yuv420,
                BitDepth::Ten,
                PlaneLayout::SemiPlanar,
            )
        })
        .unwrap();
        let i422 = FrameLayout::new(&settings(
            (1918, 1080),
            ChromaSubsampling::Yuv422,
            BitDepth::Ten,
            PlaneLayout::Planar,
        ))
        .unwrap();

        assert_eq!(nv12.planes.len(), 2);
        assert_eq!(nv12.planes[0].stride, 1920);
        assert_eq!(nv12.planes[1].offset, 1920 * 1080);
        assert_eq!(nv12.planes[1].height, 540);
        assert_eq!(nv12.size, 1920 * 1080 * 3 / 2);
        assert_eq!(p010.planes[0].stride, 3840);
        assert_eq!(p010.alignment, SampleAlignment::High);
        assert_eq!(p010.sample_shift(), 6);
        assert_eq!(i422.alignment, SampleAlignment::Low);
        assert_eq!(i422.sample_shift(), 0);
        assert_eq!(i422.planes[0].stride, 3840);
        assert_eq!(i422.planes[1].width, 959);
        assert_eq!(i422.planes[1].stride, 2048);
        assert_eq!(i422.planes[2].offset, i422.planes[1].offset + 2048 * 1080);
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let base = settings(
            (16, 16),
            ChromaSubsampling::Yuv420,
            BitDepth::Eight,
            PlaneLayout::SemiPlanar,
        );

        assert_eq!(
            FrameLayout::new(&ConvertSettings {
                row_alignment: 48,
                ..base
            }),
            Err(ScaleError::RowAlignmentInvalid(48))
        );
        assert_eq!(
            ConvertSettings {
                chroma_filter: ScaleFilter::EwaLanczos,
                format: PixelFormat {
                    chroma: ChromaSubsampling::Yuv422,
                    depth: BitDepth::Eight,
                },
                ..base
            }
            .validate(),
            Err(ScaleError::PolarChromaFilterUnsupported {
                filter: ScaleFilter::EwaLanczos,
                chroma: ChromaSubsampling::Yuv422,
            })
        );
        assert_eq!(
            ConvertSettings {
                chroma_filter: ScaleFilter::EwaLanczos,
                ..base
            }
            .validate(),
            Ok(())
        );
        assert_eq!(
            ConvertSettings {
                chroma_filter: ScaleFilter::EwaRobidoux,
                format: PixelFormat {
                    chroma: ChromaSubsampling::Yuv444,
                    depth: BitDepth::Eight,
                },
                ..base
            }
            .validate(),
            Ok(())
        );
        assert_eq!(
            ConvertSettings {
                hdr: HdrLevels {
                    reference_white_nits: 0,
                    ..HdrLevels::default()
                },
                ..base
            }
            .validate(),
            Err(ScaleError::ReferenceWhiteOutOfRange(0))
        );
        assert_eq!(
            ConvertSettings {
                hdr: HdrLevels {
                    peak_nits: 10_001,
                    ..HdrLevels::default()
                },
                ..base
            }
            .validate(),
            Err(ScaleError::PeakLuminanceOutOfRange(10_001))
        );
        assert_eq!(
            ConvertSettings {
                resolution: Resolution::new(0, 16),
                ..base
            }
            .validate(),
            Err(ScaleError::EmptySource(Resolution::new(0, 16)))
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
            PlaneLayout::SemiPlanar,
        );
        let ten = ConvertSettings {
            format: PixelFormat {
                chroma: ChromaSubsampling::Yuv420,
                depth: BitDepth::Ten,
            },
            ..eight
        };
        let mut converter = ColorConverter::new(&pipelines, &gpu.device, eight).unwrap();
        let stale = converter.create_frame(&gpu.device);
        let source = testing::view(&testing::upload(gpu, &Image::pattern(16, 16, 0)));

        let reconfigured = converter.configure(&gpu.device, ten).unwrap();
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let result = converter.encode(&gpu.device, &mut encoder, &source, &stale);

        assert_eq!(reconfigured, Reconfigured::Tables);
        assert_eq!(result, Err(ScaleError::FrameLayoutMismatch));
    }
}
