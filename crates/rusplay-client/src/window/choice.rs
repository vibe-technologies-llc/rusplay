use rusplay_config::{DisplayOutputConfig, DisplayTargetConfig};
use rusplay_scale::ColorVolume;

use crate::{
    DisplayImage, FrameColor,
    display::resolve_target,
    window::output::{Display, SurfaceOutput},
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Admission {
    Reject,
    Draw,
    Switch(SurfaceOutput),
}

#[derive(Debug)]
pub(crate) struct OutputChoice {
    display: Display,
    active: SurfaceOutput,
    staged: Option<SurfaceOutput>,
}

impl OutputChoice {
    pub(crate) fn new(display: Display, active: SurfaceOutput) -> Self {
        Self {
            display,
            active,
            staged: None,
        }
    }

    pub(crate) fn observe(
        &mut self,
        image: &DisplayImage,
        overrides: &DisplayTargetConfig,
        config: DisplayOutputConfig,
        capabilities: impl FnOnce() -> Vec<wgpu::SurfaceFormatCapabilities>,
    ) -> Option<ColorVolume> {
        let display = Display::Managed(resolve_target(image, overrides).ok()?);
        if display == self.display {
            return None;
        }

        let output = SurfaceOutput::choose(&capabilities(), &display, config).ok()?;
        let intended = self.staged.unwrap_or(self.active);
        self.display = display;
        if output == intended {
            return None;
        }

        self.staged = (output != self.active).then_some(output);

        Some(output.encoding.target)
    }

    pub(crate) fn accepts(&self, color: FrameColor) -> bool {
        carries(color, &self.active) || self.staged.is_some_and(|staged| carries(color, &staged))
    }

    pub(crate) fn admit(&mut self, color: FrameColor) -> Admission {
        if let Some(staged) = self.staged
            && carries(color, &staged)
        {
            self.active = staged;
            self.staged = None;

            return Admission::Switch(staged);
        }

        if carries(color, &self.active) {
            Admission::Draw
        } else {
            Admission::Reject
        }
    }
}

fn carries(color: FrameColor, output: &SurfaceOutput) -> bool {
    color.mapped_to(&output.encoding.target) == color
}

pub(crate) fn follow_choice<T: Clone + PartialEq>(entered: &[T], current: Option<&T>) -> Option<T> {
    match current {
        Some(held) if entered.is_empty() || entered.contains(held) => Some(held.clone()),
        _ => entered.last().cloned(),
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, ColorSpace, PixelFormat, TransferFunction};

    use super::*;
    use crate::{
        ClientError, LuminanceRange, Luminances,
        window::output::{Display, Signal},
    };

    fn capabilities() -> Vec<wgpu::SurfaceFormatCapabilities> {
        vec![
            wgpu::SurfaceFormatCapabilities {
                format: wgpu::TextureFormat::Bgra8Unorm,
                color_spaces: wgpu::SurfaceColorSpaces::SRGB,
            },
            wgpu::SurfaceFormatCapabilities {
                format: wgpu::TextureFormat::Rgb10a2Unorm,
                color_spaces: wgpu::SurfaceColorSpaces::SRGB | wgpu::SurfaceColorSpaces::BT2100_PQ,
            },
        ]
    }

    fn sdr_image(reference_nits: f64) -> DisplayImage {
        DisplayImage {
            primaries: Some(ColorPrimaries::Bt709),
            target_primaries: Some(ColorPrimaries::Bt709),
            transfer: Some(TransferFunction::Gamma22),
            luminances: Some(Luminances {
                min_nits: 0.2,
                max_nits: 250.0,
                reference_nits,
            }),
            target_luminance: None,
        }
    }

    fn hdr_image() -> DisplayImage {
        DisplayImage {
            primaries: Some(ColorPrimaries::Bt2020),
            target_primaries: Some(ColorPrimaries::Bt2020),
            transfer: Some(TransferFunction::Pq),
            luminances: Some(Luminances {
                min_nits: 0.0,
                max_nits: 10_000.0,
                reference_nits: 203.0,
            }),
            target_luminance: Some(LuminanceRange {
                min_nits: 0.0005,
                max_nits: 1000.0,
            }),
        }
    }

    fn started_on(image: &DisplayImage) -> OutputChoice {
        let overrides = DisplayTargetConfig::default();
        let config = DisplayOutputConfig::default();
        let display = Display::Managed(resolve_target(image, &overrides).unwrap());
        let active = SurfaceOutput::choose(&capabilities(), &display, config).unwrap();

        OutputChoice::new(display, active)
    }

    fn observe(choice: &mut OutputChoice, image: &DisplayImage) -> Option<ColorVolume> {
        choice.observe(
            image,
            &DisplayTargetConfig::default(),
            DisplayOutputConfig::default(),
            capabilities,
        )
    }

    fn frame_for(target: &ColorVolume) -> FrameColor {
        FrameColor::of_stream(ColorSpace::default(), PixelFormat::default()).mapped_to(target)
    }

    #[test]
    fn an_unchanged_description_changes_nothing() {
        let mut choice = started_on(&sdr_image(80.0));

        assert_eq!(observe(&mut choice, &sdr_image(80.0)), None);
        assert_eq!(choice.staged, None);
    }

    #[test]
    fn a_description_that_cannot_be_resolved_changes_nothing() {
        let mut choice = started_on(&hdr_image());
        let overrides = DisplayTargetConfig {
            peak_nits: Some(1),
            ..DisplayTargetConfig::default()
        };

        let dim = DisplayImage {
            target_luminance: Some(LuminanceRange {
                min_nits: 2.0,
                max_nits: 100.0,
            }),
            ..sdr_image(80.0)
        };

        let target = choice.observe(
            &dim,
            &overrides,
            DisplayOutputConfig::default(),
            capabilities,
        );

        assert!(matches!(
            resolve_target(&dim, &overrides),
            Err(ClientError::TargetBlackNotBelowPeak { .. })
        ));
        assert_eq!(target, None);
        assert_eq!(choice.staged, None);
    }

    #[test]
    fn a_different_output_stages_its_swapchain_and_reports_the_new_target() {
        let mut choice = started_on(&sdr_image(80.0));

        let target = observe(&mut choice, &hdr_image()).unwrap();

        assert!(target.is_hdr());
        assert_eq!(choice.staged.unwrap().encoding.target, target);
        assert_eq!(choice.active.encoding.signal, Signal::Srgb);
    }

    #[test]
    fn a_brighter_reference_white_on_the_same_kind_of_output_is_a_new_target() {
        let mut choice = started_on(&sdr_image(80.0));

        let target = observe(&mut choice, &sdr_image(120.0)).unwrap();

        assert!((target.reference_white_nits - 120.0).abs() < f64::EPSILON);
        assert!(choice.staged.is_some());
    }

    #[test]
    fn a_description_that_resolves_to_the_same_output_is_remembered_without_staging() {
        let mut choice = started_on(&sdr_image(80.0));
        let quieter = DisplayImage {
            luminances: Some(Luminances {
                min_nits: 0.3,
                max_nits: 400.0,
                reference_nits: 80.0,
            }),
            ..sdr_image(80.0)
        };

        assert_eq!(observe(&mut choice, &quieter), None);
        assert_eq!(choice.staged, None);
        assert_eq!(observe(&mut choice, &quieter), None);
    }

    #[test]
    fn a_staged_output_waits_for_a_frame_mapped_to_its_target() {
        let mut choice = started_on(&sdr_image(80.0));
        let old = choice.active.encoding.target;
        let new = observe(&mut choice, &hdr_image()).unwrap();

        assert_eq!(choice.admit(frame_for(&old)), Admission::Draw);
        assert!(choice.accepts(frame_for(&old)));
        assert!(choice.accepts(frame_for(&new)));
        assert_eq!(choice.active.encoding.target, old);

        let switch = choice.admit(frame_for(&new));

        assert!(matches!(switch, Admission::Switch(output) if output.encoding.target == new));
        assert_eq!(choice.active.encoding.target, new);
        assert_eq!(choice.staged, None);
        assert_eq!(choice.admit(frame_for(&new)), Admission::Draw);
        assert!(!choice.accepts(frame_for(&old)));
        assert_eq!(choice.admit(frame_for(&old)), Admission::Reject);
    }

    #[test]
    fn returning_to_the_active_output_before_any_frame_unstages_it() {
        let mut choice = started_on(&sdr_image(80.0));
        let old = choice.active.encoding.target;
        let away = observe(&mut choice, &hdr_image()).unwrap();

        let back = observe(&mut choice, &sdr_image(80.0));

        assert_eq!(back, Some(old));
        assert_eq!(choice.staged, None);
        assert!(!choice.accepts(frame_for(&away)));
        assert_eq!(choice.admit(frame_for(&old)), Admission::Draw);
    }

    #[test]
    fn a_frame_for_a_superseded_stage_is_rejected() {
        let mut choice = started_on(&sdr_image(80.0));
        let first = observe(&mut choice, &sdr_image(120.0)).unwrap();
        let second = observe(&mut choice, &hdr_image()).unwrap();

        assert!(!choice.accepts(frame_for(&first)));
        assert_eq!(choice.admit(frame_for(&first)), Admission::Reject);
        assert!(choice.accepts(frame_for(&second)));
    }

    #[test]
    fn a_target_maps_a_frame_idempotently() {
        let target = ColorVolume::srgb_display();
        let color = frame_for(&target);

        assert_eq!(color.mapped_to(&target), color);
    }

    #[test]
    fn the_only_entered_output_is_followed() {
        assert_eq!(follow_choice(&[7], None), Some(7));
        assert_eq!(follow_choice(&[7], Some(&7)), Some(7));
    }

    #[test]
    fn the_followed_output_is_kept_while_the_surface_is_still_on_it() {
        assert_eq!(follow_choice(&[1, 2], Some(&1)), Some(1));
        assert_eq!(follow_choice(&[1, 2], Some(&2)), Some(2));
    }

    #[test]
    fn leaving_the_followed_output_moves_to_the_latest_entered() {
        assert_eq!(follow_choice(&[1, 2, 3], Some(&9)), Some(3));
        assert_eq!(follow_choice(&[2], Some(&1)), Some(2));
    }

    #[test]
    fn with_no_output_entered_the_last_one_is_kept() {
        assert_eq!(follow_choice::<u32>(&[], Some(&4)), Some(4));
        assert_eq!(follow_choice::<u32>(&[], None), None);
    }

    #[test]
    fn with_nothing_followed_yet_the_latest_entered_is_chosen() {
        assert_eq!(follow_choice(&[5, 6], None), Some(6));
    }
}
