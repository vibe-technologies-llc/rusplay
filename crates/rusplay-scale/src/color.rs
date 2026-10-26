mod spaces;
mod tone;

#[cfg(test)]
pub(crate) mod reference;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tone_tests;

use rusplay_core::{
    Chromaticities, ColorPrimaries, ColorSpace, GamutMapMode, HdrLevels, Matrix3, ToneMapOperator,
    TransferFunction, WhiteAdaptation, bradford_adaptation,
};
use wgpu::util::DeviceExt;

pub use crate::color::{
    spaces::WorkingSpace,
    tone::{DynamicPeak, ToneCurve, ToneMapping},
};
use crate::{
    Reconfigured, ScaleError,
    gpu::{self, ScalePipelines, buffer_group, uniform_buffer},
    scaler::GroupCache,
};

pub(crate) const BOUNDARY_STEPS: u32 = 16;
pub(crate) const COMPRESSION_POWER: f64 = 1.2;
pub(crate) const CONTAINMENT_TOLERANCE: f64 = 1e-6;
pub(crate) const HISTOGRAM_BINS: u32 = 256;
pub(crate) const PEAK_TOLERANCE: f64 = 1e-6;
const TONE_STATE_WORDS: usize = 12;

const MAX_NITS: f64 = HdrLevels::MAX_NITS as f64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorVolume {
    pub primaries: ColorPrimaries,
    pub transfer: TransferFunction,
    pub reference_white_nits: f64,
    pub peak_nits: f64,
    pub black_nits: f64,
}

impl ColorVolume {
    pub const SDR_REFERENCE_WHITE_NITS: f64 = HdrLevels::BT2408_REFERENCE_WHITE_NITS as f64;

    pub const fn srgb_display() -> Self {
        Self {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferFunction::Srgb,
            reference_white_nits: Self::SDR_REFERENCE_WHITE_NITS,
            peak_nits: Self::SDR_REFERENCE_WHITE_NITS,
            black_nits: 0.0,
        }
    }

    pub fn of_content(color: &ColorSpace) -> Self {
        let white = f64::from(color.hdr.reference_white_nits);

        Self {
            primaries: color.primaries,
            transfer: color.transfer,
            reference_white_nits: white,
            peak_nits: if color.transfer.is_hdr() {
                color.hdr.content_peak_nits()
            } else {
                white
            },
            black_nits: color
                .hdr
                .mastering
                .map_or(0.0, |mastering| mastering.min_nits.nits()),
        }
    }

    pub const fn is_hdr(&self) -> bool {
        self.transfer.is_hdr()
    }

    pub const fn display_peak_nits(&self) -> f64 {
        if self.is_hdr() {
            self.peak_nits
        } else {
            self.reference_white_nits
        }
    }

    pub fn relative_peak(&self) -> f64 {
        if self.is_hdr() {
            self.peak_nits / self.reference_white_nits
        } else {
            1.0
        }
    }

    fn validate(&self) -> Result<(), ScaleError> {
        let in_range = |nits: f64| nits > 0.0 && nits <= MAX_NITS;

        if !in_range(self.reference_white_nits) {
            return Err(ScaleError::ReferenceWhiteNitsOutOfRange(
                self.reference_white_nits,
            ));
        }
        if !in_range(self.peak_nits) {
            return Err(ScaleError::PeakNitsOutOfRange(self.peak_nits));
        }
        if !(0.0..self.peak_nits).contains(&self.black_nits) {
            return Err(ScaleError::BlackNitsOutOfRange {
                black_nits: self.black_nits,
                peak_nits: self.peak_nits,
            });
        }

        Ok(())
    }
}

impl Default for ColorVolume {
    fn default() -> Self {
        Self::srgb_display()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GamutMapping {
    pub mode: GamutMapMode,
    pub knee: f64,
}

impl GamutMapping {
    pub const DEFAULT_KNEE: f64 = 0.8;

    fn validate(&self) -> Result<(), ScaleError> {
        if (0.0..1.0).contains(&self.knee) {
            Ok(())
        } else {
            Err(ScaleError::GamutKneeOutOfRange(self.knee))
        }
    }
}

impl Default for GamutMapping {
    fn default() -> Self {
        Self {
            mode: GamutMapMode::Perceptual,
            knee: Self::DEFAULT_KNEE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColorMapSettings {
    pub source: ColorVolume,
    pub target: ColorVolume,
    pub tone: ToneMapping,
    pub gamut: GamutMapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MappingPath {
    Matrix,
    Perceptual,
    Clip,
    Desaturate,
    Highlight,
}

impl MappingPath {
    const fn code(self) -> u32 {
        match self {
            Self::Matrix => 0,
            Self::Perceptual => 1,
            Self::Clip => 2,
            Self::Desaturate => 3,
            Self::Highlight => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneStage {
    pub curve: ToneCurve,
    pub dynamic_peak: Option<DynamicPeak>,
}

impl ToneStage {
    const fn operator_code(&self) -> u32 {
        match self.curve.operator {
            ToneMapOperator::Bt2390 => 1,
            ToneMapOperator::Bt2446a => 2,
            ToneMapOperator::Spline => 3,
            ToneMapOperator::Reinhard => 4,
            ToneMapOperator::Clip => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColorPlan {
    pub(crate) path: MappingPath,
    pub(crate) space: WorkingSpace,
    pub(crate) gain: f64,
    pub(crate) target_peak: f64,
    pub(crate) knee: f64,
    pub(crate) pq_scale: f64,
    pub(crate) source_to_target: Matrix3,
    pub(crate) source_to_work: Matrix3,
    pub(crate) work_to_target: Matrix3,
    pub(crate) work_to_source: Matrix3,
    pub(crate) xyz_to_lms: Matrix3,
    pub(crate) lms_to_xyz: Matrix3,
    pub(crate) lms_to_lab: Matrix3,
    pub(crate) lab_to_lms: Matrix3,
    pub(crate) target_luma: [f64; 3],
    pub(crate) tone: Option<ToneStage>,
    pub(crate) source_white_nits: f64,
    pub(crate) target_white_nits: f64,
    pub(crate) tone_xyz_to_lms: Matrix3,
    pub(crate) tone_lms_to_xyz: Matrix3,
    pub(crate) tone_lms_to_itp: Matrix3,
    pub(crate) tone_itp_to_lms: Matrix3,
}

impl ColorPlan {
    pub fn new(settings: &ColorMapSettings) -> Result<Self, ScaleError> {
        let ColorMapSettings {
            source,
            target,
            tone,
            gamut,
        } = settings;
        source.validate()?;
        target.validate()?;
        gamut.validate()?;
        if let Some(dynamic_peak) = tone.dynamic_peak {
            dynamic_peak.validate()?;
        }

        let source_gamut = source.primaries.chromaticities();
        let target_gamut = target.primaries.chromaticities();
        let target_white = target.reference_white_nits;
        let source_white = if source.is_hdr() {
            source.reference_white_nits
        } else {
            target_white
        };

        let space = if source.is_hdr() || target.is_hdr() {
            WorkingSpace::Ictcp
        } else {
            WorkingSpace::Oklab
        };
        let d65 = Chromaticities::BT709.white().xyz();
        let source_to_work =
            bradford_adaptation(source_gamut.white().xyz(), d65) * source_gamut.rgb_to_xyz();
        let work_to_target =
            target_gamut.xyz_to_rgb() * bradford_adaptation(d65, target_gamut.white().xyz());
        let work_to_source =
            source_gamut.xyz_to_rgb() * bradford_adaptation(d65, source_gamut.white().xyz());
        let relative = source_gamut.rgb_to_rgb(&target_gamut, WhiteAdaptation::Bradford);
        let contained = relative
            .rows()
            .iter()
            .flatten()
            .all(|value| *value >= -CONTAINMENT_TOLERANCE);

        let (path, source_to_target) = match gamut.mode {
            GamutMapMode::Perceptual if !contained => (MappingPath::Perceptual, relative),
            GamutMapMode::Relative if !contained => (MappingPath::Clip, relative),
            GamutMapMode::Desaturate if !contained => (MappingPath::Desaturate, relative),
            GamutMapMode::Highlight if !contained => (MappingPath::Highlight, relative),
            GamutMapMode::Perceptual
            | GamutMapMode::Relative
            | GamutMapMode::Desaturate
            | GamutMapMode::Highlight => (MappingPath::Matrix, relative),
            GamutMapMode::Absolute => (
                MappingPath::Clip,
                source_gamut.rgb_to_rgb(&target_gamut, WhiteAdaptation::None),
            ),
            GamutMapMode::Saturation => (MappingPath::Clip, Matrix3::IDENTITY),
        };

        let xyz_to_lms = space.xyz_to_lms();
        let lms_to_lab = space.lms_to_lab();
        let tone_xyz_to_lms = WorkingSpace::Ictcp.xyz_to_lms();
        let tone_lms_to_itp = WorkingSpace::Ictcp.lms_to_lab();
        let target_peak_nits = target.display_peak_nits();
        let needs_tone_mapping = tone.dynamic_peak.is_some()
            || source.peak_nits > target_peak_nits * (1.0 + PEAK_TOLERANCE);
        let tone = (source.is_hdr() && needs_tone_mapping).then_some(ToneStage {
            curve: ToneCurve {
                operator: tone.operator,
                source_peak_nits: source.peak_nits.max(target_peak_nits),
                source_black_nits: source.black_nits,
                target_peak_nits,
                target_black_nits: target.black_nits,
            },
            dynamic_peak: tone.dynamic_peak,
        });

        Ok(Self {
            path,
            space,
            gain: source_white / target_white,
            target_peak: target.relative_peak(),
            knee: gamut.knee,
            pq_scale: target_white / MAX_NITS,
            source_to_target,
            source_to_work,
            work_to_target,
            work_to_source,
            xyz_to_lms,
            lms_to_xyz: inverse(xyz_to_lms)?,
            lms_to_lab,
            lab_to_lms: inverse(lms_to_lab)?,
            target_luma: target_gamut.rgb_to_xyz().rows()[1],
            tone,
            source_white_nits: source_white,
            target_white_nits: target_white,
            tone_xyz_to_lms,
            tone_lms_to_xyz: inverse(tone_xyz_to_lms)?,
            tone_lms_to_itp,
            tone_itp_to_lms: inverse(tone_lms_to_itp)?,
        })
    }

    pub const fn tone(&self) -> Option<ToneStage> {
        self.tone
    }

    pub fn is_identity(&self) -> bool {
        self.path == MappingPath::Matrix
            && self.tone.is_none()
            && (self.gain - 1.0).abs() <= PEAK_TOLERANCE
            && self.source_to_target.max_difference(&Matrix3::IDENTITY) <= CONTAINMENT_TOLERANCE
    }

    pub const fn path(&self) -> MappingPath {
        self.path
    }

    pub const fn space(&self) -> WorkingSpace {
        self.space
    }

    pub const fn source_to_target(&self) -> Matrix3 {
        self.source_to_target
    }

    pub const fn target_peak(&self) -> f64 {
        self.target_peak
    }

    fn params_bytes(&self) -> Vec<u8> {
        let mut words = Vec::with_capacity(PARAMS_WORDS);
        for matrix in [
            self.source_to_target,
            self.source_to_work,
            self.work_to_target,
            self.work_to_source,
            self.xyz_to_lms,
            self.lms_to_xyz,
            self.lms_to_lab,
            self.lab_to_lms,
            self.tone_xyz_to_lms,
            self.tone_lms_to_xyz,
            self.tone_lms_to_itp,
            self.tone_itp_to_lms,
        ] {
            for row in matrix.rows() {
                words.extend(row.map(float));
                words.push(0);
            }
        }
        words.extend(self.target_luma.map(float));
        words.push(0);
        words.extend([
            float(self.target_peak),
            float(self.knee),
            float(self.pq_scale),
            float(self.gain),
        ]);
        let curve = self.tone.map(|stage| stage.curve);
        let dynamic = self.tone.and_then(|stage| stage.dynamic_peak);
        words.extend([
            float(self.source_white_nits),
            float(self.target_white_nits),
            0,
            0,
        ]);
        words.extend([
            float(curve.map_or(1.0, |curve| curve.target_peak_nits)),
            float(curve.map_or(0.0, |curve| curve.source_black_nits)),
            float(curve.map_or(0.0, |curve| curve.target_black_nits)),
            float(curve.map_or(1.0, |curve| curve.source_peak_nits)),
        ]);
        words.extend([
            float(dynamic.map_or(1.0, |dynamic| dynamic.percentile / 100.0)),
            float(dynamic.map_or(1.0, |dynamic| f64::from(dynamic.smoothing_frames))),
            float(dynamic.map_or(1.0, |dynamic| dynamic.scene_threshold_db)),
            0,
        ]);
        words.extend([
            self.path.code(),
            self.space.code(),
            self.tone.map_or(0, |stage| stage.operator_code()),
            u32::from(dynamic.is_some()),
        ]);

        words.iter().flat_map(|word| word.to_ne_bytes()).collect()
    }

    fn tone_state_bytes(&self) -> Vec<u8> {
        let mut words = Vec::with_capacity(TONE_STATE_WORDS);
        words.extend(self.tone.map_or([0; 8], |stage| stage.curve.words()));
        words.extend([0, 0, 0, 0]);

        words.iter().flat_map(|word| word.to_ne_bytes()).collect()
    }
}

const PARAMS_WORDS: usize = 12 * 12 + 4 * 4 + 4;

fn inverse(matrix: Matrix3) -> Result<Matrix3, ScaleError> {
    matrix.inverse().ok_or(ScaleError::SingularColorMatrix)
}

fn float(value: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let narrowed = value as f32;
    narrowed.to_bits()
}

#[derive(Debug)]
struct MapperBuffers {
    map_group: wgpu::BindGroup,
    analysis_group: wgpu::BindGroup,
    tone_state: wgpu::Buffer,
}

impl MapperBuffers {
    fn new(pipelines: &ScalePipelines, device: &wgpu::Device, plan: &ColorPlan) -> Self {
        let params = uniform_buffer(device, "rusplay-color-params", &plan.params_bytes());
        let tone_state = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rusplay-color-tone-state"),
            contents: &plan.tone_state_bytes(),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let histogram = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rusplay-color-histogram"),
            size: u64::from(HISTOGRAM_BINS) * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });

        Self {
            map_group: buffer_group(
                device,
                "rusplay-color-map",
                &pipelines.color.params_layout,
                &[&params, &tone_state],
            ),
            analysis_group: buffer_group(
                device,
                "rusplay-color-analysis",
                &pipelines.color.analysis_layout,
                &[&params, &tone_state, &histogram],
            ),
            tone_state,
        }
    }
}

#[derive(Debug)]
pub struct ColorMapper {
    pipelines: ScalePipelines,
    settings: ColorMapSettings,
    plan: ColorPlan,
    buffers: MapperBuffers,
    sources: GroupCache<wgpu::TextureView>,
    destinations: GroupCache<wgpu::TextureView>,
}

impl ColorMapper {
    pub fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: ColorMapSettings,
    ) -> Result<Self, ScaleError> {
        let plan = ColorPlan::new(&settings)?;

        Ok(Self {
            pipelines: pipelines.clone(),
            buffers: MapperBuffers::new(pipelines, device, &plan),
            settings,
            plan,
            sources: GroupCache::default(),
            destinations: GroupCache::default(),
        })
    }

    pub fn settings(&self) -> ColorMapSettings {
        self.settings
    }

    pub fn plan(&self) -> &ColorPlan {
        &self.plan
    }

    pub fn tone_state(&self) -> &wgpu::Buffer {
        &self.buffers.tone_state
    }

    pub fn configure(
        &mut self,
        device: &wgpu::Device,
        settings: ColorMapSettings,
    ) -> Result<Reconfigured, ScaleError> {
        if settings == self.settings {
            return Ok(Reconfigured::Unchanged);
        }
        let plan = ColorPlan::new(&settings)?;

        self.buffers = MapperBuffers::new(&self.pipelines, device, &plan);
        self.plan = plan;
        self.settings = settings;
        Ok(Reconfigured::Parameters)
    }

    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        let pipelines = &self.pipelines;
        let source_group = self
            .sources
            .get_or_create(source, || pipelines.source_group(device, source));
        let target_group = self
            .destinations
            .get_or_create(target, || pipelines.destination_group(device, target));
        let source_size = source.texture().size();
        let size = target.texture().size();

        if self
            .plan
            .tone
            .is_some_and(|stage| stage.dynamic_peak.is_some())
        {
            gpu::dispatch(
                encoder,
                "rusplay-color-histogram",
                &pipelines.color.histogram,
                &[&source_group, &self.buffers.analysis_group],
                (source_size.width, source_size.height),
            );
            gpu::dispatch(
                encoder,
                "rusplay-color-peak",
                &pipelines.color.peak,
                &[&self.buffers.analysis_group],
                (1, 1),
            );
        }
        gpu::dispatch(
            encoder,
            "rusplay-color-map",
            &pipelines.color.map,
            &[&source_group, &target_group, &self.buffers.map_group],
            (size.width, size.height),
        );
    }
}
