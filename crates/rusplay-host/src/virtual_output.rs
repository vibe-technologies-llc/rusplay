use std::time::{Duration, Instant};

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
        self.offer(request);

        request
    }

    pub(crate) fn offer(&self, request: OutputRequest) {
        if let Some(offer) = &self.offer {
            offer.offer(request);
        }
    }
}

pub(crate) fn select_output(display: &ClientDisplay, limits: &CaptureConfig) -> OutputRequest {
    let min = limits.virtual_min_resolution;
    let max = limits.virtual_max_resolution;
    let fitted = fit_within(grow_to(display.resolution(), min), max);

    OutputRequest {
        resolution: Resolution::new(
            fitted.width.max(min.width.min(max.width)),
            fitted.height.max(min.height.min(max.height)),
        ),
        frame_rate: limit_rate(display, limits.virtual_max_refresh_hz),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResizeGate {
    min_interval: Duration,
    last_applied: Option<Instant>,
    pending: Option<ClientDisplay>,
}

impl ResizeGate {
    pub(crate) fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_applied: None,
            pending: None,
        }
    }

    pub(crate) fn submit(&mut self, display: ClientDisplay) -> bool {
        self.pending.replace(display).is_some()
    }

    pub(crate) fn due(&mut self, now: Instant) -> Option<ClientDisplay> {
        let display = self.pending?;
        if self.next_allowed().is_some_and(|allowed| now < allowed) {
            return None;
        }

        self.pending = None;
        self.last_applied = Some(now);

        Some(display)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.pending?;

        Some(self.next_allowed().unwrap_or_else(Instant::now))
    }

    pub(crate) fn reset(&mut self) {
        self.last_applied = None;
        self.pending = None;
    }

    fn next_allowed(&self) -> Option<Instant> {
        self.last_applied.map(|applied| applied + self.min_interval)
    }
}

fn grow_to(wanted: Resolution, min: Resolution) -> Resolution {
    if wanted.width >= min.width && wanted.height >= min.height {
        return wanted;
    }

    let width_is_tighter = u64::from(min.width) * u64::from(wanted.height)
        > u64::from(min.height) * u64::from(wanted.width);

    let (width, height) = if width_is_tighter {
        let height =
            (u64::from(wanted.height) * u64::from(min.width)).div_ceil(u64::from(wanted.width));
        (u64::from(min.width), height)
    } else {
        let width =
            (u64::from(wanted.width) * u64::from(min.height)).div_ceil(u64::from(wanted.height));
        (width, u64::from(min.height))
    };

    Resolution::new(
        u32::try_from(width).unwrap_or(u32::MAX),
        u32::try_from(height).unwrap_or(u32::MAX),
    )
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

pub(crate) fn stream_rate(
    display: &ClientDisplay,
    max_hertz: u32,
    source: Option<FrameRate>,
) -> FrameRate {
    let client = limit_rate(display, max_hertz);
    let slower = |rate: FrameRate| rate.frame_interval().unwrap_or(Duration::ZERO);

    match source.filter(|source| source.is_valid()) {
        Some(source) if slower(source) > slower(client) => source,
        Some(_) | None => client,
    }
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
    fn the_stream_rate_is_the_slower_of_the_client_display_and_the_source() {
        let client = display(2560, 1440, 144_000);

        assert_eq!(
            stream_rate(&client, 240, Some(FrameRate::integer(60))),
            FrameRate::integer(60)
        );
        assert_eq!(
            stream_rate(&client, 240, Some(FrameRate::integer(240))),
            FrameRate::integer(144)
        );
        assert_eq!(stream_rate(&client, 240, None), FrameRate::integer(144));
        assert_eq!(stream_rate(&client, 120, None), FrameRate::integer(120));
        assert_eq!(
            stream_rate(&client, 240, Some(FrameRate::new(0, 1))),
            FrameRate::integer(144)
        );
    }

    #[test]
    fn a_refresh_at_the_limit_is_kept() {
        let request = select_output(&display(1920, 1080, 240_000), &limits(3840, 2160, 240));

        assert_eq!(request.frame_rate, FrameRate::integer(240));
    }

    #[test]
    fn a_display_below_the_minimum_grows_to_it_keeping_its_aspect_ratio() {
        let small = select_output(&display(100, 50, 60_000), &CaptureConfig::default());
        let narrow = select_output(&display(150, 300, 60_000), &CaptureConfig::default());
        let exact = select_output(&display(200, 200, 60_000), &CaptureConfig::default());

        assert_eq!(small.resolution, Resolution::new(400, 200));
        assert_eq!(narrow.resolution, Resolution::new(200, 400));
        assert_eq!(exact.resolution, Resolution::new(200, 200));
    }

    #[test]
    fn a_degenerate_display_still_lands_inside_both_limits() {
        let limits = limits(1920, 1080, 60);
        let extremes = [
            (1, 1),
            (1, 16_384),
            (16_384, 1),
            (3, 16_384),
            (16_384, 16_384),
            (199, 199),
        ];

        for (width, height) in extremes {
            let request = select_output(&display(width, height, 60_000), &limits);

            assert!((200..=1920).contains(&request.resolution.width));
            assert!((200..=1080).contains(&request.resolution.height));
        }
    }

    #[test]
    fn the_configured_minimum_is_the_one_applied() {
        let config = CaptureConfig {
            virtual_min_resolution: Resolution::new(640, 360),
            ..CaptureConfig::default()
        };

        let request = select_output(&display(320, 180, 60_000), &config);

        assert_eq!(request.resolution, Resolution::new(640, 360));
    }

    const INTERVAL: Duration = Duration::from_millis(250);
    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn the_first_display_is_applied_at_once_and_the_next_waits_for_the_interval() {
        let start = Instant::now();
        let mut gate = ResizeGate::new(INTERVAL);

        gate.submit(display(1000, 700, 60_000));
        let first = gate.due(start);
        gate.submit(display(1100, 700, 60_000));
        let early = gate.due(start + 249 * MS);
        let on_time = gate.due(start + 250 * MS);

        assert_eq!(first, Some(display(1000, 700, 60_000)));
        assert_eq!(early, None);
        assert_eq!(on_time, Some(display(1100, 700, 60_000)));
    }

    #[test]
    fn a_flood_of_displays_applies_only_the_newest_once_per_interval() {
        let start = Instant::now();
        let mut gate = ResizeGate::new(INTERVAL);
        let mut applied = Vec::new();
        let mut replaced = 0;

        for step in 0..10_000u32 {
            let now = start + MS * (step / 10);
            replaced += usize::from(gate.submit(display(1000 + step % 5000, 700, 60_000)));
            if let Some(display) = gate.due(now) {
                applied.push((now, display));
            }
        }

        assert!(applied.len() <= 1000 / 250 + 1, "{}", applied.len());
        assert!(
            applied
                .windows(2)
                .all(|pair| pair[1].0.duration_since(pair[0].0) >= INTERVAL)
        );
        assert!(replaced >= 10_000 - applied.len() - 1);
    }

    #[test]
    fn nothing_is_due_without_a_pending_display_and_the_deadline_follows_the_interval() {
        let start = Instant::now();
        let mut gate = ResizeGate::new(INTERVAL);

        assert_eq!(gate.due(start), None);
        assert_eq!(gate.next_deadline(), None);

        gate.submit(display(1000, 700, 60_000));
        gate.due(start);
        gate.submit(display(1100, 700, 60_000));

        assert_eq!(gate.next_deadline(), Some(start + INTERVAL));
    }

    #[test]
    fn a_reset_forgets_the_pending_display_and_applies_the_next_at_once() {
        let start = Instant::now();
        let mut gate = ResizeGate::new(INTERVAL);
        gate.submit(display(1000, 700, 60_000));
        gate.due(start);
        gate.submit(display(1100, 700, 60_000));

        gate.reset();
        gate.submit(display(1200, 700, 60_000));

        assert_eq!(gate.due(start + MS), Some(display(1200, 700, 60_000)));
    }
}
