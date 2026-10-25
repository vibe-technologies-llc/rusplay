use std::collections::VecDeque;

use rusplay_core::{Resolution, ScaleFilter};

use crate::{
    Geometry, Kernel, ScaleError, TapTable,
    gpu::{
        self, INTERMEDIATE_FORMAT, ScalePipelines, buffer_group, storage_buffer, to_bytes,
        uniform_buffer,
    },
};

const CACHED_VIEW_GROUPS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    const fn index(self) -> u32 {
        match self {
            Self::Horizontal => 0,
            Self::Vertical => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleSettings {
    pub filter: ScaleFilter,
    pub source: Resolution,
    pub target: Resolution,
}

impl ScaleSettings {
    fn validate(&self) -> Result<(), ScaleError> {
        if self.source.pixel_count() == 0 {
            return Err(ScaleError::EmptySource(self.source));
        }
        if self.target.pixel_count() == 0 {
            return Err(ScaleError::EmptyTarget(self.target));
        }
        if self.filter.is_polar() {
            return Err(ScaleError::PolarPassUnavailable(self.filter));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reconfigured {
    Unchanged,
    Rebuilt,
}

#[derive(Debug)]
pub struct Scaler {
    pipelines: ScalePipelines,
    settings: ScaleSettings,
    plan: SeparablePlan,
    sources: ViewGroups,
    destinations: ViewGroups,
}

impl Scaler {
    pub fn new(
        pipelines: &ScalePipelines,
        device: &wgpu::Device,
        settings: ScaleSettings,
    ) -> Result<Self, ScaleError> {
        settings.validate()?;

        Ok(Self {
            pipelines: pipelines.clone(),
            settings,
            plan: SeparablePlan::new(pipelines, device, &settings),
            sources: ViewGroups::default(),
            destinations: ViewGroups::default(),
        })
    }

    pub fn settings(&self) -> ScaleSettings {
        self.settings
    }

    pub fn configure(
        &mut self,
        device: &wgpu::Device,
        settings: ScaleSettings,
    ) -> Result<Reconfigured, ScaleError> {
        if settings == self.settings {
            return Ok(Reconfigured::Unchanged);
        }
        settings.validate()?;

        self.plan = SeparablePlan::new(&self.pipelines, device, &settings);
        self.settings = settings;
        Ok(Reconfigured::Rebuilt)
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
        let plan = &self.plan;

        gpu::dispatch(
            encoder,
            "rusplay-resample-horizontal",
            &pipelines.separable,
            &[
                &source_group,
                &plan.intermediate_destination,
                &plan.horizontal,
            ],
            plan.intermediate_size,
        );
        gpu::dispatch(
            encoder,
            "rusplay-resample-vertical",
            &pipelines.separable,
            &[&plan.intermediate_source, &target_group, &plan.vertical],
            resolution_pair(self.settings.target),
        );
    }
}

#[derive(Debug)]
struct SeparablePlan {
    horizontal: wgpu::BindGroup,
    vertical: wgpu::BindGroup,
    intermediate_size: (u32, u32),
    intermediate_source: wgpu::BindGroup,
    intermediate_destination: wgpu::BindGroup,
}

impl SeparablePlan {
    fn new(pipelines: &ScalePipelines, device: &wgpu::Device, settings: &ScaleSettings) -> Self {
        let kernel = Kernel::new(settings.filter);
        debug_assert_eq!(kernel.geometry(), Geometry::Separable);

        let source = resolution_pair(settings.source);
        let target = resolution_pair(settings.target);
        let intermediate_size = (target.0, source.1);

        let intermediate = gpu::storage_texture(
            device,
            "rusplay-resample-intermediate",
            intermediate_size,
            INTERMEDIATE_FORMAT,
        );

        Self {
            horizontal: pass_group(
                pipelines,
                device,
                &TapTable::build(kernel, source.0, target.0),
                Axis::Horizontal,
                source,
                intermediate_size,
            ),
            vertical: pass_group(
                pipelines,
                device,
                &TapTable::build(kernel, source.1, target.1),
                Axis::Vertical,
                intermediate_size,
                target,
            ),
            intermediate_size,
            intermediate_source: pipelines.source_group(device, &intermediate),
            intermediate_destination: pipelines.destination_group(device, &intermediate),
        }
    }
}

fn pass_group(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    table: &TapTable,
    axis: Axis,
    source_size: (u32, u32),
    target_size: (u32, u32),
) -> wgpu::BindGroup {
    let firsts = storage_buffer(device, "rusplay-resample-firsts", &table.firsts);
    let weights = storage_buffer(device, "rusplay-resample-weights", &table.weights);
    let params = uniform_buffer(
        device,
        "rusplay-resample-params",
        &to_bytes(&[
            source_size.0,
            source_size.1,
            target_size.0,
            target_size.1,
            table.taps_per_pixel,
            axis.index(),
            0,
            0,
        ]),
    );

    buffer_group(
        device,
        "rusplay-resample-table",
        &pipelines.separable_layout,
        &[&firsts, &weights, &params],
    )
}

pub(crate) const fn resolution_pair(resolution: Resolution) -> (u32, u32) {
    (resolution.width, resolution.height)
}

#[derive(Debug, Default)]
pub(crate) struct ViewGroups {
    entries: VecDeque<(wgpu::TextureView, wgpu::BindGroup)>,
}

impl ViewGroups {
    pub(crate) fn get_or_create(
        &mut self,
        view: &wgpu::TextureView,
        create: impl FnOnce() -> wgpu::BindGroup,
    ) -> wgpu::BindGroup {
        if let Some((_, group)) = self.entries.iter().find(|(cached, _)| cached == view) {
            return group.clone();
        }

        let group = create();
        if self.entries.len() == CACHED_VIEW_GROUPS {
            self.entries.pop_front();
        }
        self.entries.push_back((view.clone(), group.clone()));
        group
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        reference::resample_separable,
        testing::{self, Gpu, Image, RGBA16F_TOLERANCE},
    };

    const SEPARABLE_FILTERS: [ScaleFilter; 7] = [
        ScaleFilter::Bilinear,
        ScaleFilter::Hermite,
        ScaleFilter::CatmullRom,
        ScaleFilter::Mitchell,
        ScaleFilter::Lanczos2,
        ScaleFilter::Lanczos3,
        ScaleFilter::Lanczos4,
    ];

    fn settings(filter: ScaleFilter, source: (u32, u32), target: (u32, u32)) -> ScaleSettings {
        ScaleSettings {
            filter,
            source: Resolution::new(source.0, source.1),
            target: Resolution::new(target.0, target.1),
        }
    }

    fn expected(image: &Image, settings: &ScaleSettings) -> Image {
        let kernel = Kernel::new(settings.filter);
        let horizontal = TapTable::build(kernel, settings.source.width, settings.target.width);
        let vertical = TapTable::build(kernel, settings.source.height, settings.target.height);

        resample_separable(
            &resample_separable(image, &horizontal, Axis::Horizontal),
            &vertical,
            Axis::Vertical,
        )
    }

    fn scale(gpu: &Gpu, scaler: &mut Scaler, image: &Image) -> Image {
        let target_size = resolution_pair(scaler.settings().target);

        let source = testing::upload(gpu, image);
        let target = testing::storage_target(gpu, target_size.0, target_size.1);

        testing::submit(gpu, |encoder| {
            scaler.encode(
                &gpu.device,
                encoder,
                &testing::view(&source),
                &testing::view(&target),
            );
        });
        testing::download(gpu, &target)
    }

    fn assert_matches_reference(settings: ScaleSettings) {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);
        let mut scaler = Scaler::new(&pipelines, &gpu.device, settings).unwrap();
        let image = Image::pattern(settings.source.width, settings.source.height, 7);

        let difference =
            scale(gpu, &mut scaler, &image).max_difference(&expected(&image, &settings));

        assert!(
            difference <= RGBA16F_TOLERANCE,
            "{settings:?}: max difference {difference}"
        );
    }

    #[test]
    fn hermite_downscale_matches_reference() {
        assert_matches_reference(settings(ScaleFilter::Hermite, (97, 61), (40, 27)));
    }

    #[test]
    fn lanczos3_upscale_matches_reference() {
        assert_matches_reference(settings(ScaleFilter::Lanczos3, (37, 29), (91, 70)));
    }

    #[test]
    fn every_separable_filter_matches_reference_both_ways() {
        for filter in SEPARABLE_FILTERS {
            assert_matches_reference(settings(filter, (64, 48), (23, 17)));
            assert_matches_reference(settings(filter, (23, 17), (64, 48)));
        }
    }

    #[test]
    fn mixed_direction_scale_matches_reference() {
        assert_matches_reference(settings(ScaleFilter::CatmullRom, (50, 20), (31, 45)));
    }

    #[test]
    fn identity_scale_reproduces_the_source() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);
        let mut scaler = Scaler::new(
            &pipelines,
            &gpu.device,
            settings(ScaleFilter::Lanczos3, (33, 21), (33, 21)),
        )
        .unwrap();
        let image = Image::pattern(33, 21, 3);

        assert!(scale(gpu, &mut scaler, &image).max_difference(&image) <= 1.0 / 1024.0);
    }

    #[test]
    fn reconfiguring_rebuilds_only_on_change() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);
        let first = settings(ScaleFilter::Hermite, (64, 48), (32, 24));
        let second = settings(ScaleFilter::Mitchell, (40, 30), (70, 50));
        let mut scaler = Scaler::new(&pipelines, &gpu.device, first).unwrap();

        let unchanged = scaler.configure(&gpu.device, first).unwrap();
        let rebuilt = scaler.configure(&gpu.device, second).unwrap();
        let image = Image::pattern(40, 30, 11);
        let difference = scale(gpu, &mut scaler, &image).max_difference(&expected(&image, &second));

        assert_eq!(unchanged, Reconfigured::Unchanged);
        assert_eq!(rebuilt, Reconfigured::Rebuilt);
        assert_eq!(scaler.settings(), second);
        assert!(difference <= RGBA16F_TOLERANCE);
    }

    #[test]
    fn frame_views_reuse_their_bind_groups() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);
        let mut scaler = Scaler::new(
            &pipelines,
            &gpu.device,
            settings(ScaleFilter::Hermite, (16, 16), (8, 8)),
        )
        .unwrap();
        let source = testing::view(&testing::upload(gpu, &Image::pattern(16, 16, 1)));
        let target = testing::view(&testing::storage_target(gpu, 8, 8));

        testing::submit(gpu, |encoder| {
            for _ in 0..3 {
                scaler.encode(&gpu.device, encoder, &source, &target);
            }
        });

        assert_eq!(scaler.sources.len(), 1);
        assert_eq!(scaler.destinations.len(), 1);
    }

    #[test]
    fn view_group_cache_is_bounded() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);
        let mut groups = ViewGroups::default();

        for _ in 0..CACHED_VIEW_GROUPS + 3 {
            let view = testing::view(&testing::upload(gpu, &Image::pattern(4, 4, 0)));
            groups.get_or_create(&view, || pipelines.source_group(&gpu.device, &view));
        }

        assert_eq!(groups.len(), CACHED_VIEW_GROUPS);
    }

    #[test]
    fn empty_resolutions_are_rejected() {
        let Some(gpu) = testing::gpu() else {
            return;
        };

        let pipelines = ScalePipelines::new(&gpu.device);

        let empty_source = Scaler::new(
            &pipelines,
            &gpu.device,
            settings(ScaleFilter::Hermite, (0, 10), (8, 8)),
        );
        let empty_target = Scaler::new(
            &pipelines,
            &gpu.device,
            settings(ScaleFilter::Hermite, (8, 8), (8, 0)),
        );

        assert_eq!(
            empty_source.unwrap_err(),
            ScaleError::EmptySource(Resolution::new(0, 10))
        );
        assert_eq!(
            empty_target.unwrap_err(),
            ScaleError::EmptyTarget(Resolution::new(8, 0))
        );
    }
}
