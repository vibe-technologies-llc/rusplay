use rusplay_config::Config;
use rusplay_core::{FrameRate, HdrLevels, Luminance, Resolution};
use rusplay_protocol::{ClientDisplay, DisplayVolume, StreamDescription};
use rusplay_scale::ColorVolume;

use crate::{
    ClientError,
    display::{DisplayImage, resolve_target},
};

const FALLBACK_RESOLUTION: Resolution = Resolution::new(1920, 1080);
const MILLIHERTZ_PER_HERTZ: u64 = 1000;

pub fn announced_display(
    resolution: Resolution,
    refresh: FrameRate,
    volume: &ColorVolume,
) -> Result<ClientDisplay, ClientError> {
    let axis = 1..=StreamDescription::MAX_DIMENSION;

    Ok(ClientDisplay::new(
        Resolution::new(
            resolution.width.clamp(*axis.start(), *axis.end()),
            resolution.height.clamp(*axis.start(), *axis.end()),
        ),
        refresh_millihertz(refresh),
        display_volume(volume),
    )?)
}

pub(crate) fn default_display(config: &Config) -> Result<ClientDisplay, ClientError> {
    let volume = resolve_target(&DisplayImage::default(), &config.color.target)
        .unwrap_or_else(|_| ColorVolume::srgb_display());

    announced_display(
        config.video.resolution.unwrap_or(FALLBACK_RESOLUTION),
        config.video.frame_rate,
        &volume,
    )
}

fn refresh_millihertz(refresh: FrameRate) -> u32 {
    let millihertz =
        u64::from(refresh.numerator) * MILLIHERTZ_PER_HERTZ / u64::from(refresh.denominator.max(1));

    u32::try_from(millihertz).map_or(ClientDisplay::MAX_REFRESH_MILLIHERTZ, |millihertz| {
        millihertz.clamp(
            ClientDisplay::MIN_REFRESH_MILLIHERTZ,
            ClientDisplay::MAX_REFRESH_MILLIHERTZ,
        )
    })
}

fn display_volume(volume: &ColorVolume) -> DisplayVolume {
    let reference_white_nits = whole_nits(volume.reference_white_nits);
    let darkest_below_white =
        f64::from(reference_white_nits) - 1.0 / f64::from(Luminance::UNITS_PER_NIT);
    let black = Luminance::from_nits(volume.black_nits.clamp(0.0, darkest_below_white))
        .unwrap_or(Luminance::ZERO);

    DisplayVolume {
        primaries: volume.primaries,
        transfer: volume.transfer,
        reference_white_nits,
        peak_nits: whole_nits(volume.peak_nits),
        black,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn whole_nits(nits: f64) -> u32 {
    let range = f64::from(1)..=f64::from(HdrLevels::MAX_NITS);

    (nits.round().clamp(*range.start(), *range.end()) as u32).max(1)
}

#[cfg(test)]
mod tests {
    use rusplay_core::{ColorPrimaries, TransferFunction};

    use super::*;

    fn hdr_volume() -> ColorVolume {
        ColorVolume {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            reference_white_nits: 203.0,
            peak_nits: 1000.0,
            black_nits: 0.005,
        }
    }

    #[test]
    fn a_swapchain_size_refresh_and_volume_are_announced_as_they_are() {
        let display = announced_display(
            Resolution::new(3440, 1440),
            FrameRate::new(143_981, 1000),
            &hdr_volume(),
        )
        .unwrap();

        assert_eq!(display.resolution(), Resolution::new(3440, 1440));
        assert_eq!(display.refresh_millihertz(), 143_981);
        assert_eq!(display.volume().primaries, ColorPrimaries::Bt2020);
        assert_eq!(display.volume().transfer, TransferFunction::Pq);
        assert_eq!(display.volume().reference_white_nits, 203);
        assert_eq!(display.volume().peak_nits, 1000);
        assert_eq!(display.volume().black, Luminance::from_nits(0.005).unwrap());
    }

    #[test]
    fn a_size_beyond_the_protocol_is_clamped_instead_of_refused() {
        let display = announced_display(
            Resolution::new(0, u32::MAX),
            FrameRate::integer(60),
            &ColorVolume::srgb_display(),
        )
        .unwrap();

        assert_eq!(
            display.resolution(),
            Resolution::new(1, StreamDescription::MAX_DIMENSION)
        );
    }

    #[test]
    fn a_refresh_beyond_the_protocol_is_clamped() {
        let refresh = |rate| {
            announced_display(
                Resolution::new(1920, 1080),
                rate,
                &ColorVolume::srgb_display(),
            )
            .unwrap()
            .refresh_millihertz()
        };

        assert_eq!(
            refresh(FrameRate::new(0, 1)),
            ClientDisplay::MIN_REFRESH_MILLIHERTZ
        );
        assert_eq!(refresh(FrameRate::new(60, 0)), 60_000);
        assert_eq!(
            refresh(FrameRate::new(u32::MAX, 1)),
            ClientDisplay::MAX_REFRESH_MILLIHERTZ
        );
    }

    #[test]
    fn a_volume_outside_the_protocol_is_clamped_into_it() {
        let mut volume = hdr_volume();
        volume.reference_white_nits = f64::NAN;
        volume.peak_nits = 1e9;
        volume.black_nits = 5000.0;

        let display =
            announced_display(Resolution::new(1920, 1080), FrameRate::integer(60), &volume)
                .unwrap();

        assert_eq!(display.volume().reference_white_nits, 1);
        assert_eq!(display.volume().peak_nits, HdrLevels::MAX_NITS);
        assert!(display.volume().black.units() < Luminance::UNITS_PER_NIT);
    }

    #[test]
    fn the_default_display_follows_the_client_config() {
        let mut config = Config::default();
        config.video.resolution = Some(Resolution::new(2560, 1440));
        config.video.frame_rate = FrameRate::integer(144);

        let display = default_display(&config).unwrap();
        let unset = default_display(&Config::default()).unwrap();

        assert_eq!(display.resolution(), Resolution::new(2560, 1440));
        assert_eq!(display.refresh_millihertz(), 144_000);
        assert_eq!(unset.resolution(), FALLBACK_RESOLUTION);
        assert_eq!(unset.volume().transfer, TransferFunction::Srgb);
        assert_eq!(unset.volume().reference_white_nits, 203);
    }
}
