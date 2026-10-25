use rusplay_core::ScaleFilter;

use crate::{
    Kernel, ScaleSettings,
    gpu::{self, ScalePipelines, ShaderPass, buffer_group, storage_buffer, uniform_buffer},
    scaler::resolution_pair,
    shaping::Shaping,
};

pub(crate) const RADIAL_LUT_SIZE: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PolarGeometry {
    pub source: (u32, u32),
    pub target: (u32, u32),
    pub ratio: (f32, f32),
    pub stretch: (f32, f32),
    pub radius: f32,
    pub phase: (f32, f32),
}

impl PolarGeometry {
    pub(crate) fn new(settings: &ScaleSettings) -> Self {
        let source = resolution_pair(settings.source);
        let target = resolution_pair(settings.target);
        let ratio = (
            axis_ratio(source.0, target.0),
            axis_ratio(source.1, target.1),
        );

        Self::with_ratio(settings.filter, source, target, ratio, (0.0, 0.0))
    }

    pub(crate) fn with_ratio(
        filter: ScaleFilter,
        source: (u32, u32),
        target: (u32, u32),
        ratio: (f32, f32),
        phase: (f32, f32),
    ) -> Self {
        #[allow(clippy::cast_possible_truncation)]
        let radius = Kernel::new(filter).radius() as f32;

        Self {
            source,
            target,
            ratio,
            stretch: (ratio.0.max(1.0), ratio.1.max(1.0)),
            radius,
            phase,
        }
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn axis_ratio(source: u32, target: u32) -> f32 {
    (f64::from(source) / f64::from(target)) as f32
}

pub(crate) fn radial_lut(kernel: Kernel, size: u32) -> Vec<f32> {
    let last = f64::from(size - 1);

    #[allow(clippy::cast_possible_truncation)]
    let lut = (0..size)
        .map(|index| kernel.weight(f64::from(index) / last * kernel.radius()) as f32)
        .collect();
    lut
}

#[derive(Debug)]
pub(crate) struct PolarPlan {
    lut: wgpu::Buffer,
    geometry: PolarGeometry,
    pass: ShaderPass,
    group: wgpu::BindGroup,
}

impl PolarPlan {
    pub(crate) fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: &ScaleSettings,
    ) -> Self {
        Self::build(
            pipelines,
            device,
            pipelines.polar.clone(),
            settings.filter,
            PolarGeometry::new(settings),
            &settings.polar_shaping(),
        )
    }

    pub(crate) fn chroma(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        filter: ScaleFilter,
        geometry: PolarGeometry,
    ) -> Self {
        let unshaped = Shaping {
            antiringing: 0.0,
            sigmoid: None,
            sigmoidize_input: false,
            unsigmoidize_output: false,
        };

        Self::build(
            pipelines,
            device,
            pipelines.convert.polar_chroma.clone(),
            filter,
            geometry,
            &unshaped,
        )
    }

    fn build(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        pass: ShaderPass,
        filter: ScaleFilter,
        geometry: PolarGeometry,
        shaping: &Shaping,
    ) -> Self {
        let lut = storage_buffer(
            device,
            "rusplay-resample-radial-lut",
            &radial_lut(Kernel::new(filter), RADIAL_LUT_SIZE),
        );
        let group = Self::group(pipelines, device, &lut, &geometry, shaping);

        Self {
            lut,
            geometry,
            pass,
            group,
        }
    }

    pub(crate) fn set_parameters(
        &mut self,
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: &ScaleSettings,
    ) {
        self.group = Self::group(
            pipelines,
            device,
            &self.lut,
            &self.geometry,
            &settings.polar_shaping(),
        );
    }

    pub(crate) fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source_group: &wgpu::BindGroup,
        target_group: &wgpu::BindGroup,
    ) {
        gpu::dispatch(
            encoder,
            "rusplay-resample-polar",
            &self.pass,
            &[source_group, target_group, &self.group],
            self.geometry.target,
        );
    }

    fn group(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        lut: &wgpu::Buffer,
        geometry: &PolarGeometry,
        shaping: &Shaping,
    ) -> wgpu::BindGroup {
        let pass = [
            geometry.source.0,
            geometry.source.1,
            geometry.target.0,
            geometry.target.1,
            geometry.ratio.0.to_bits(),
            geometry.ratio.1.to_bits(),
            geometry.stretch.0.to_bits(),
            geometry.stretch.1.to_bits(),
            geometry.radius.to_bits(),
            RADIAL_LUT_SIZE,
            geometry.phase.0.to_bits(),
            geometry.phase.1.to_bits(),
        ];
        let bytes: Vec<u8> = shaping
            .words()
            .iter()
            .chain(&pass)
            .flat_map(|word| word.to_ne_bytes())
            .collect();
        let params = uniform_buffer(device, "rusplay-resample-polar-params", &bytes);

        buffer_group(
            device,
            "rusplay-resample-polar",
            &pipelines.polar_layout,
            &[lut, &params],
        )
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::{Resolution, ScaleFilter};

    use super::*;
    use crate::{
        Reconfigured, ScaleError, Scaler, Sigmoid,
        reference::resample_polar,
        testing::{self, Image, RGBA16F_TOLERANCE},
    };

    const POLAR_FILTERS: [ScaleFilter; 6] = [
        ScaleFilter::EwaLanczos,
        ScaleFilter::EwaLanczosSharp,
        ScaleFilter::EwaGinseng,
        ScaleFilter::EwaHann,
        ScaleFilter::EwaRobidoux,
        ScaleFilter::EwaRobidouxSharp,
    ];
    const SIGMOID_TOLERANCE: f32 = 8.0 / 1024.0;

    fn settings(filter: ScaleFilter, source: (u32, u32), target: (u32, u32)) -> ScaleSettings {
        ScaleSettings {
            filter,
            source: Resolution::new(source.0, source.1),
            target: Resolution::new(target.0, target.1),
            antiringing: 0.0,
            sigmoid: None,
        }
    }

    fn assert_matches_reference(settings: ScaleSettings, tolerance: f32) {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);
        let mut scaler = Scaler::new(&pipelines, &gpu.device, settings).unwrap();
        let image = Image::pattern(settings.source.width, settings.source.height, 13);

        let difference = testing::scale(gpu, &mut scaler, &image)
            .max_difference(&resample_polar(&image, &settings));

        assert!(
            difference <= tolerance,
            "{settings:?}: max difference {difference}"
        );
    }

    #[test]
    fn every_polar_filter_matches_reference_both_ways() {
        for filter in POLAR_FILTERS {
            assert_matches_reference(settings(filter, (23, 19), (61, 47)), RGBA16F_TOLERANCE);
            assert_matches_reference(settings(filter, (61, 47), (23, 19)), RGBA16F_TOLERANCE);
        }
    }

    #[test]
    fn anisotropic_polar_scale_matches_reference() {
        assert_matches_reference(
            settings(ScaleFilter::EwaLanczos, (90, 20), (30, 50)),
            RGBA16F_TOLERANCE,
        );
    }

    #[test]
    fn wide_polar_downscale_matches_reference() {
        assert_matches_reference(
            settings(ScaleFilter::EwaRobidoux, (160, 120), (37, 28)),
            RGBA16F_TOLERANCE,
        );
    }

    #[test]
    fn polar_shaping_matches_reference() {
        assert_matches_reference(
            ScaleSettings {
                antiringing: 1.0,
                ..settings(ScaleFilter::EwaLanczosSharp, (25, 20), (64, 51))
            },
            RGBA16F_TOLERANCE,
        );
        assert_matches_reference(
            ScaleSettings {
                antiringing: 0.5,
                sigmoid: Some(Sigmoid::default()),
                ..settings(ScaleFilter::EwaLanczos, (25, 20), (64, 51))
            },
            SIGMOID_TOLERANCE,
        );
    }

    #[test]
    fn switching_between_geometries_rebuilds_the_plan() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);
        let separable = settings(ScaleFilter::Lanczos3, (20, 16), (48, 40));
        let polar = settings(ScaleFilter::EwaLanczos, (20, 16), (48, 40));
        let mut scaler = Scaler::new(&pipelines, &gpu.device, separable).unwrap();

        let reconfigured = scaler.configure(&gpu.device, polar).unwrap();
        let image = Image::pattern(20, 16, 2);
        let difference = testing::scale(gpu, &mut scaler, &image)
            .max_difference(&resample_polar(&image, &polar));

        assert_eq!(reconfigured, Reconfigured::Tables);
        assert!(difference <= RGBA16F_TOLERANCE);
    }

    #[test]
    fn polar_settings_are_validated() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);

        let result = Scaler::new(
            &pipelines,
            &gpu.device,
            settings(ScaleFilter::EwaHann, (0, 4), (4, 4)),
        );

        assert_eq!(
            result.unwrap_err(),
            ScaleError::EmptySource(Resolution::new(0, 4))
        );
    }

    #[test]
    fn lut_spans_the_kernel_radius() {
        let kernel = Kernel::new(ScaleFilter::EwaLanczos);

        let lut = radial_lut(kernel, RADIAL_LUT_SIZE);

        assert_eq!(lut.len(), RADIAL_LUT_SIZE as usize);
        assert!((lut[0] - 1.0).abs() < 1e-6);
        assert!(lut[lut.len() - 1].abs() < 1e-6);
    }
}
