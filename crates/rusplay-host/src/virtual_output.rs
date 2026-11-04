use rusplay_capture::{OutputOffer, OutputRequest};
use rusplay_config::CaptureConfig;
use rusplay_core::{FrameRate, Resolution};
use rusplay_protocol::ClientDisplay;

const MILLIHERTZ_PER_HERTZ: u32 = 1000;

#[derive(Debug, Clone)]
pub(crate) struct VirtualOutput {
    pub limits: CaptureConfig,
    pub offer: Option<OutputOffer>,
}

impl VirtualOutput {
    pub(crate) fn select(&self, display: &ClientDisplay) -> OutputRequest {
        let request = select_output(display, &self.limits);
        if let Some(offer) = &self.offer {
            offer.offer(request);
        }

        request
    }
}

pub(crate) fn select_output(display: &ClientDisplay, limits: &CaptureConfig) -> OutputRequest {
    OutputRequest {
        resolution: fit_within(display.resolution(), limits.virtual_max_resolution),
        frame_rate: limit_rate(display, limits.virtual_max_refresh_hz),
    }
}

fn fit_within(wanted: Resolution, max: Resolution) -> Resolution {
    if wanted.width <= max.width && wanted.height <= max.height {
        return wanted;
    }

    let width_is_tighter = u64::from(max.width) * u64::from(wanted.height)
        < u64::from(max.height) * u64::from(wanted.width);

    let (width, height) = if width_is_tighter {
        let height = u64::from(wanted.height) * u64::from(max.width) / u64::from(wanted.width);
        (u64::from(max.width), height)
    } else {
        let width = u64::from(wanted.width) * u64::from(max.height) / u64::from(wanted.height);
        (width, u64::from(max.height))
    };

    Resolution::new(
        u32::try_from(width).map_or(max.width, |width| width.clamp(1, max.width)),
        u32::try_from(height).map_or(max.height, |height| height.clamp(1, max.height)),
    )
}

fn limit_rate(display: &ClientDisplay, max_hertz: u32) -> FrameRate {
    if display.refresh_millihertz() > max_hertz.saturating_mul(MILLIHERTZ_PER_HERTZ) {
        FrameRate::integer(max_hertz)
    } else {
        display.frame_rate()
    }
}

#[cfg(test)]
mod tests {
    use rusplay_protocol::DisplayVolume;

    use super::*;

    fn display(width: u32, height: u32, millihertz: u32) -> ClientDisplay {
        ClientDisplay::new(
            Resolution::new(width, height),
            millihertz,
            DisplayVolume::srgb(),
        )
        .unwrap()
    }

    fn limits(width: u32, height: u32, hertz: u32) -> CaptureConfig {
        CaptureConfig {
            virtual_max_resolution: Resolution::new(width, height),
            virtual_max_refresh_hz: hertz,
            ..CaptureConfig::default()
        }
    }

    #[test]
    fn a_display_inside_the_limits_is_taken_as_it_is() {
        let request = select_output(&display(2560, 1440, 143_981), &limits(3840, 2160, 240));

        assert_eq!(request.resolution, Resolution::new(2560, 1440));
        assert_eq!(request.frame_rate, FrameRate::new(143_981, 1000));
    }

    #[test]
    fn the_default_limits_admit_an_8k_display() {
        let request = select_output(&display(7680, 4320, 60_000), &CaptureConfig::default());

        assert_eq!(request.resolution, Resolution::new(7680, 4320));
    }

    #[test]
    fn a_display_beyond_the_limits_is_fitted_keeping_its_aspect_ratio() {
        let wide = select_output(&display(5120, 1440, 60_000), &limits(3840, 2160, 240));
        let tall = select_output(&display(1440, 5120, 60_000), &limits(3840, 2160, 240));
        let large = select_output(&display(7680, 4320, 60_000), &limits(3840, 2160, 240));

        assert_eq!(wide.resolution, Resolution::new(3840, 1080));
        assert_eq!(tall.resolution, Resolution::new(607, 2160));
        assert_eq!(large.resolution, Resolution::new(3840, 2160));
    }

    #[test]
    fn a_hostile_display_never_exceeds_the_limits_on_either_axis() {
        let max = limits(1920, 1080, 60);
        let extremes = [
            (16_384, 16_384),
            (16_384, 1),
            (1, 16_384),
            (16_384, 2),
            (3, 16_384),
        ];

        for (width, height) in extremes {
            let request = select_output(&display(width, height, 60_000), &max);

            assert!((1..=1920).contains(&request.resolution.width));
            assert!((1..=1080).contains(&request.resolution.height));
        }
    }

    #[test]
    fn a_refresh_above_the_limit_is_capped_to_the_limit() {
        let request = select_output(&display(1920, 1080, 1_000_000), &limits(3840, 2160, 240));

        assert_eq!(request.frame_rate, FrameRate::integer(240));
    }

    #[test]
    fn a_refresh_at_the_limit_is_kept() {
        let request = select_output(&display(1920, 1080, 240_000), &limits(3840, 2160, 240));

        assert_eq!(request.frame_rate, FrameRate::integer(240));
    }
}
