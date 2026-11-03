use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use rusplay_client::{
    ClientStats, ColorVolume, FrameSink, OverlayText, PresentationStats, PresentedFrame,
    StatsPhase, StatsSchedule, stats_json_line, unix_millis_now,
};
use rusplay_config::NetworkConfig;

const SAMPLE_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub struct FrameTimings {
    frames: AtomicU64,
    decode_micros: AtomicU64,
    present_micros: AtomicU64,
    peak_micros: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TimingTotals {
    frames: u64,
    decode_micros: u64,
    present_micros: u64,
}

impl FrameTimings {
    fn record(&self, frame: &PresentedFrame) {
        let decode = frame
            .decoded_at
            .saturating_duration_since(frame.received_at);
        let present = frame
            .submitted_at
            .saturating_duration_since(frame.decoded_at);
        let total = frame
            .submitted_at
            .saturating_duration_since(frame.received_at);

        self.frames.fetch_add(1, Ordering::Relaxed);
        self.decode_micros
            .fetch_add(micros(decode), Ordering::Relaxed);
        self.present_micros
            .fetch_add(micros(present), Ordering::Relaxed);
        self.peak_micros.fetch_max(micros(total), Ordering::Relaxed);
    }

    fn totals(&self) -> TimingTotals {
        TimingTotals {
            frames: self.frames.load(Ordering::Relaxed),
            decode_micros: self.decode_micros.load(Ordering::Relaxed),
            present_micros: self.present_micros.load(Ordering::Relaxed),
        }
    }

    fn take_peak(&self) -> u64 {
        self.peak_micros.swap(0, Ordering::Relaxed)
    }
}

pub struct TimingSink<S> {
    inner: S,
    timings: Arc<FrameTimings>,
}

impl<S> TimingSink<S> {
    pub fn new(inner: S, timings: Arc<FrameTimings>) -> Self {
        Self { inner, timings }
    }
}

impl<S: FrameSink> FrameSink for TimingSink<S> {
    fn present(&mut self, frame: PresentedFrame) {
        self.timings.record(&frame);
        self.inner.present(frame);
    }

    fn color_target(&self) -> Option<ColorVolume> {
        self.inner.color_target()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientLatency {
    pub decode: Duration,
    pub present: Duration,
    pub peak: Duration,
}

impl ClientLatency {
    pub fn total(self) -> Duration {
        self.decode + self.present
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndToEnd {
    pub receive: Option<Duration>,
    pub decode: Option<Duration>,
    pub present: Duration,
}

impl EndToEnd {
    fn from_stats(stats: &ClientStats) -> Option<Self> {
        Some(Self {
            receive: stats.capture_to_receive,
            decode: stats.capture_to_decode,
            present: stats.capture_to_present?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamRates {
    pub received_fps: f64,
    pub presented_fps: f64,
    pub frames_lost: f64,
    pub frames_recovered: f64,
    pub frames_superseded: f64,
    pub keyframe_requests: f64,
    pub decode_errors: f64,
    pub audio_underruns: f64,
    pub latency: Option<ClientLatency>,
    pub end_to_end: Option<EndToEnd>,
    pub display: Option<DisplayTiming>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayTiming {
    pub submit_to_display: Duration,
    pub capture_to_display: Option<Duration>,
    pub refresh: Option<Duration>,
}

impl DisplayTiming {
    pub fn from_presentation(stats: &PresentationStats) -> Option<Self> {
        Some(Self {
            submit_to_display: stats.display_latency?,
            capture_to_display: stats.capture_to_display,
            refresh: stats.refresh,
        })
    }

    fn rate_label(self) -> String {
        self.refresh.map_or_else(
            || "VRR".to_owned(),
            |refresh| format!("{:.0} HZ", 1.0 / refresh.as_secs_f64()),
        )
    }

    fn summary(self) -> String {
        let rate = self.refresh.map_or_else(
            || "variable refresh".to_owned(),
            |refresh| format!("{:.0} Hz", 1.0 / refresh.as_secs_f64()),
        );
        let submit = format!(
            "{:.1} ms submit to display at {rate}",
            millis(self.submit_to_display)
        );

        match self.capture_to_display {
            Some(capture) => format!("{submit}, {:.1} ms capture to display", millis(capture)),
            None => submit,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Reading {
    at: Instant,
    stats: ClientStats,
    totals: TimingTotals,
}

impl StreamRates {
    fn between(previous: &Reading, current: &Reading, peak_micros: u64) -> Option<Self> {
        let seconds = current
            .at
            .saturating_duration_since(previous.at)
            .as_secs_f64();
        if seconds <= 0.0 {
            return None;
        }

        let rate = |now: u64, before: u64| per_second(now.saturating_sub(before), seconds);
        let (now, before) = (&current.stats, &previous.stats);

        let frames = current.totals.frames.saturating_sub(previous.totals.frames);
        let latency = u32::try_from(frames)
            .ok()
            .filter(|frames| *frames > 0)
            .map(|frames| {
                let average = |now: u64, before: u64| {
                    Duration::from_micros(now.saturating_sub(before)) / frames
                };

                ClientLatency {
                    decode: average(current.totals.decode_micros, previous.totals.decode_micros),
                    present: average(
                        current.totals.present_micros,
                        previous.totals.present_micros,
                    ),
                    peak: Duration::from_micros(peak_micros),
                }
            });

        Some(Self {
            received_fps: rate(now.frames_received, before.frames_received),
            presented_fps: rate(now.frames_presented, before.frames_presented),
            frames_lost: rate(
                now.frames_lost + now.frames_skipped,
                before.frames_lost + before.frames_skipped,
            ),
            frames_recovered: rate(now.frames_recovered, before.frames_recovered),
            frames_superseded: rate(now.frames_superseded, before.frames_superseded),
            keyframe_requests: rate(now.keyframe_requests_sent, before.keyframe_requests_sent),
            decode_errors: rate(now.decode_errors, before.decode_errors),
            audio_underruns: rate(now.audio_underruns, before.audio_underruns),
            latency,
            end_to_end: EndToEnd::from_stats(now),
            display: None,
        })
    }

    pub fn with_presentation(mut self, stats: &PresentationStats) -> Self {
        self.display = DisplayTiming::from_presentation(stats);
        self
    }

    pub fn overlay(&self) -> OverlayText {
        let mut lines = vec![format!(
            "{:.0} FPS  RECV {:.0}  LOST {:.1}/S",
            self.presented_fps, self.received_fps, self.frames_lost
        )];

        if let Some(end_to_end) = self.end_to_end {
            lines.push(format!("E2E {:.1} MS", millis(end_to_end.present)));
            lines.extend(
                end_to_end
                    .receive
                    .map(|time| format!("  RX {:.1} MS", millis(time))),
            );
            lines.extend(
                end_to_end
                    .decode
                    .map(|time| format!("  DEC {:.1} MS", millis(time))),
            );
        }
        if let Some(display) = self.display {
            lines.push(format!(
                "DISPLAY +{:.1} MS  {}",
                millis(display.submit_to_display),
                display.rate_label()
            ));
            lines.extend(
                display
                    .capture_to_display
                    .map(|time| format!("  E2E {:.1} MS", millis(time))),
            );
        }
        if let Some(latency) = self.latency {
            lines.push(format!(
                "CLIENT {:.1} MS  PEAK {:.1}",
                millis(latency.total()),
                millis(latency.peak)
            ));
        }

        OverlayText::new(lines)
    }

    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{:.0} fps", self.presented_fps)];

        if let Some(end_to_end) = self.end_to_end {
            parts.push(end_to_end.summary());
        }
        if let Some(display) = self.display {
            parts.push(display.summary());
        }
        if let Some(latency) = self.latency {
            parts.push(format!(
                "{:.1} ms client (decode {:.1}, present {:.1}, peak {:.1})",
                millis(latency.total()),
                millis(latency.decode),
                millis(latency.present),
                millis(latency.peak)
            ));
        }
        parts.push(format!("received {:.0} fps", self.received_fps));
        parts.push(format!("lost {:.1}/s", self.frames_lost));
        parts.push(format!("recovered {:.1}/s", self.frames_recovered));

        let occasional = [
            ("superseded", self.frames_superseded),
            ("keyframe requests", self.keyframe_requests),
            ("decode errors", self.decode_errors),
            ("audio underruns", self.audio_underruns),
        ];
        parts.extend(
            occasional
                .into_iter()
                .filter(|(_, rate)| *rate > 0.0)
                .map(|(label, rate)| format!("{label} {rate:.1}/s")),
        );

        parts.join(" · ")
    }
}

impl EndToEnd {
    fn summary(self) -> String {
        let stages = [("receive", self.receive), ("decode", self.decode)]
            .into_iter()
            .filter_map(|(label, time)| time.map(|time| format!("{label} {:.1}", millis(time))))
            .collect::<Vec<_>>();

        if stages.is_empty() {
            format!("{:.1} ms capture to present", millis(self.present))
        } else {
            format!(
                "{:.1} ms capture to present ({})",
                millis(self.present),
                stages.join(", ")
            )
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JsonReporter {
    schedule: StatsSchedule,
}

impl JsonReporter {
    pub fn new(network: &NetworkConfig, now: Instant) -> Option<Self> {
        network.stats_json.then(|| Self {
            schedule: StatsSchedule::new(network.stats_log_interval_ms, now),
        })
    }

    pub fn report_if_due(&mut self, now: Instant, stats: &ClientStats) {
        if self.schedule.take_due(now) {
            Self::emit(StatsPhase::Interval, stats);
        }
    }

    pub fn report_final(stats: &ClientStats) {
        Self::emit(StatsPhase::Final, stats);
    }

    fn emit(phase: StatsPhase, stats: &ClientStats) {
        println!("{}", stats_json_line(phase, unix_millis_now(), stats));
    }
}

#[derive(Debug)]
pub struct StatsSampler {
    timings: Arc<FrameTimings>,
    previous: Option<Reading>,
}

impl StatsSampler {
    pub fn new(timings: Arc<FrameTimings>) -> Self {
        Self {
            timings,
            previous: None,
        }
    }

    pub fn sample(&mut self, at: Instant, stats: &ClientStats) -> Option<StreamRates> {
        let current = Reading {
            at,
            stats: *stats,
            totals: self.timings.totals(),
        };
        let Some(previous) = self.previous else {
            self.previous = Some(current);
            return None;
        };

        if at.saturating_duration_since(previous.at) < SAMPLE_WINDOW {
            return None;
        }

        self.previous = Some(current);

        StreamRates::between(&previous, &current, self.timings.take_peak())
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn to_f64(value: u64) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn per_second(delta: u64, seconds: f64) -> f64 {
    to_f64(delta) / seconds
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_reporter_exists_only_with_the_option_and_follows_the_interval() {
        let start = Instant::now();
        let off = NetworkConfig::default();
        let on = NetworkConfig {
            stats_json: true,
            stats_log_interval_ms: 1_000,
            ..NetworkConfig::default()
        };

        let reporter = JsonReporter::new(&on, start).unwrap();

        assert!(JsonReporter::new(&off, start).is_none());
        let mut schedule = reporter.schedule;
        assert!(!schedule.take_due(start + Duration::from_millis(999)));
        assert!(schedule.take_due(start + Duration::from_millis(1_000)));
    }

    fn reading(at: Instant) -> Reading {
        Reading {
            at,
            stats: ClientStats::default(),
            totals: TimingTotals::default(),
        }
    }

    #[test]
    fn counters_become_per_second_rates_over_the_elapsed_time() {
        let start = Instant::now();
        let before = reading(start);
        let mut after = reading(start + Duration::from_secs(2));
        after.stats.frames_received = 130;
        after.stats.frames_presented = 120;
        after.stats.frames_lost = 3;
        after.stats.frames_skipped = 1;
        after.stats.frames_recovered = 6;
        after.stats.decode_errors = 2;

        let rates = StreamRates::between(&before, &after, 0).unwrap();

        assert_eq!(rates.presented_fps, 60.0);
        assert_eq!(rates.received_fps, 65.0);
        assert_eq!(rates.frames_lost, 2.0);
        assert_eq!(rates.frames_recovered, 3.0);
        assert_eq!(rates.decode_errors, 1.0);
        assert_eq!(rates.frames_superseded, 0.0);
        assert_eq!(rates.latency, None);
    }

    #[test]
    fn timings_average_over_the_frames_presented_in_the_window() {
        let start = Instant::now();
        let before = reading(start);
        let mut after = reading(start + Duration::from_secs(1));
        after.totals = TimingTotals {
            frames: 4,
            decode_micros: 8_000,
            present_micros: 2_000,
        };

        let latency = StreamRates::between(&before, &after, 6_500)
            .unwrap()
            .latency
            .unwrap();

        assert_eq!(latency.decode, Duration::from_millis(2));
        assert_eq!(latency.present, Duration::from_micros(500));
        assert_eq!(latency.total(), Duration::from_micros(2_500));
        assert_eq!(latency.peak, Duration::from_micros(6_500));
    }

    #[test]
    fn a_counter_that_went_backwards_reads_as_zero() {
        let start = Instant::now();
        let mut before = reading(start);
        before.stats.frames_lost = 9;
        let after = reading(start + Duration::from_secs(1));

        let rates = StreamRates::between(&before, &after, 0).unwrap();

        assert_eq!(rates.frames_lost, 0.0);
    }

    #[test]
    fn no_time_between_readings_yields_no_rates() {
        let both = reading(Instant::now());

        assert_eq!(StreamRates::between(&both, &both, 0), None);
    }

    #[test]
    fn the_sampler_baselines_first_and_reports_once_a_window_has_elapsed() {
        let mut sampler = StatsSampler::new(Arc::new(FrameTimings::default()));
        let stats = ClientStats::default();
        let start = Instant::now();

        let baseline = sampler.sample(start, &stats);
        let early = sampler.sample(start + Duration::from_millis(100), &stats);
        let due = sampler.sample(start + Duration::from_millis(600), &stats);

        assert_eq!(baseline, None);
        assert_eq!(early, None);
        assert!(due.is_some());
    }

    #[test]
    fn the_summary_lists_the_always_shown_figures_and_only_the_nonzero_extras() {
        let rates = StreamRates {
            received_fps: 60.0,
            presented_fps: 59.6,
            frames_lost: 0.0,
            frames_recovered: 1.5,
            frames_superseded: 0.0,
            keyframe_requests: 2.0,
            decode_errors: 0.0,
            audio_underruns: 0.0,
            latency: Some(ClientLatency {
                decode: Duration::from_millis(2),
                present: Duration::from_micros(500),
                peak: Duration::from_micros(6_500),
            }),
            end_to_end: None,
            display: None,
        };

        assert_eq!(
            rates.summary(),
            "60 fps · 2.5 ms client (decode 2.0, present 0.5, peak 6.5) · received 60 fps · lost 0.0/s · recovered 1.5/s · keyframe requests 2.0/s"
        );
    }

    fn measured_rates() -> StreamRates {
        let start = Instant::now();
        let before = reading(start);
        let mut after = reading(start + Duration::from_secs(1));
        after.stats.frames_presented = 60;
        after.stats.frames_received = 60;
        after.stats.capture_to_receive = Some(Duration::from_micros(3_100));
        after.stats.capture_to_decode = Some(Duration::from_micros(9_000));
        after.stats.capture_to_present = Some(Duration::from_micros(12_340));
        after.totals = TimingTotals {
            frames: 4,
            decode_micros: 8_000,
            present_micros: 2_000,
        };

        StreamRates::between(&before, &after, 6_500).unwrap()
    }

    #[test]
    fn the_capture_to_present_figure_comes_from_the_client_stats() {
        let rates = measured_rates();

        let end_to_end = rates.end_to_end.unwrap();

        assert_eq!(end_to_end.present, Duration::from_micros(12_340));
        assert_eq!(end_to_end.receive, Some(Duration::from_micros(3_100)));
        assert_eq!(end_to_end.decode, Some(Duration::from_micros(9_000)));
        assert!(
            rates
                .summary()
                .starts_with("60 fps · 12.3 ms capture to present (receive 3.1, decode 9.0) · ")
        );
    }

    #[test]
    fn without_a_clock_offset_there_is_no_end_to_end_figure() {
        let start = Instant::now();
        let rates =
            StreamRates::between(&reading(start), &reading(start + Duration::from_secs(1)), 0)
                .unwrap();

        assert_eq!(rates.end_to_end, None);
        assert!(!rates.summary().contains("capture to present"));
        assert!(
            !rates
                .overlay()
                .lines()
                .iter()
                .any(|line| line.contains("E2E"))
        );
    }

    #[test]
    fn presentation_feedback_adds_a_display_line_and_summary_part() {
        let stats = PresentationStats {
            feedback: true,
            frames_presented: 10,
            display_latency: Some(Duration::from_micros(4_200)),
            capture_to_display: Some(Duration::from_millis(21)),
            refresh: Some(Duration::from_nanos(16_666_667)),
            ..PresentationStats::default()
        };

        let rates = measured_rates().with_presentation(&stats);
        let variable = measured_rates().with_presentation(&PresentationStats {
            refresh: None,
            capture_to_display: None,
            ..stats
        });

        assert!(
            rates
                .overlay()
                .lines()
                .iter()
                .any(|line| line == "DISPLAY +4.2 MS  60 HZ")
        );
        assert!(
            rates
                .overlay()
                .lines()
                .iter()
                .any(|line| line == "  E2E 21.0 MS")
        );
        assert!(
            rates
                .summary()
                .contains("4.2 ms submit to display at 60 Hz, 21.0 ms capture to display")
        );
        assert!(
            variable
                .overlay()
                .lines()
                .iter()
                .any(|line| line == "DISPLAY +4.2 MS  VRR")
        );
        assert_eq!(
            measured_rates()
                .with_presentation(&PresentationStats::default())
                .display,
            None
        );
    }

    #[test]
    fn the_overlay_lists_rates_end_to_end_and_client_latency() {
        let overlay = measured_rates().overlay();

        assert_eq!(
            overlay.lines(),
            [
                "60 FPS  RECV 60  LOST 0.0/S",
                "E2E 12.3 MS",
                "  RX 3.1 MS",
                "  DEC 9.0 MS",
                "CLIENT 2.5 MS  PEAK 6.5",
            ]
        );
    }
}
