use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use rusplay_client::{
    ClientStats, ColorVolume, FrameSink, OverlayText, PresentationStats, PresentedFrame,
    QualityDecibels, QualityReport, QualityState, QualityUnavailableReason, StatsPhase,
    StatsSchedule, stats_json_line, unix_millis_now,
};
use rusplay_config::NetworkConfig;
use rusplay_host::host_stats_json_line;

use crate::hosting::HostStats;

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
    pub audio_buffer: Duration,
    pub latency: Option<ClientLatency>,
    pub end_to_end: Option<EndToEnd>,
    pub display: Option<DisplayTiming>,
    pub quality: Option<QualityReport>,
}

pub fn decibels_text(value: QualityDecibels) -> String {
    if value.is_lossless() {
        "lossless".to_owned()
    } else {
        format!("{:.2}", value.decibels().value())
    }
}

fn planes_text(luma: QualityDecibels, cb: QualityDecibels, cr: QualityDecibels) -> String {
    format!(
        "{} / {} / {} dB",
        decibels_text(luma),
        decibels_text(cb),
        decibels_text(cr)
    )
}

pub fn quality_rows(snapshot: &ClientStats) -> Vec<(&'static str, String)> {
    let status = match snapshot.quality {
        QualityState::Off => "Off",
        QualityState::Waiting => "Waiting for a sample…",
        QualityState::Reported => "Measuring",
        QualityState::Unavailable(QualityUnavailableReason::EncoderUnsupported) => {
            "The host encoder cannot reconstruct"
        }
        QualityState::Unavailable(QualityUnavailableReason::HostDisabled) => {
            "The host has sampling off"
        }
        QualityState::Unavailable(QualityUnavailableReason::MeasurementFailed) => {
            "The last measurement failed"
        }
    };
    let mut rows = vec![("Status", status.to_owned())];

    if let Some(report) = snapshot.quality_report {
        rows.extend([
            (
                "PSNR Y / U / V",
                planes_text(report.psnr.luma, report.psnr.cb, report.psnr.cr),
            ),
            (
                "PSNR overall",
                format!("{} dB", decibels_text(report.psnr_overall)),
            ),
            (
                "XPSNR Y / U / V",
                planes_text(report.xpsnr.luma, report.xpsnr.cb, report.xpsnr.cr),
            ),
            ("Sampled frame", report.frame.to_string()),
        ]);
        rows.extend(
            snapshot
                .quality_age
                .map(|age| ("Sample age", format!("{:.1} s", age.as_secs_f64()))),
        );
    }

    rows
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
            audio_buffer: Duration::from_micros(now.audio_jitter_target_us),
            latency,
            end_to_end: EndToEnd::from_stats(now),
            display: None,
            quality: now
                .quality_report
                .filter(|_| now.quality != QualityState::Off),
        })
    }

    pub fn with_presentation(mut self, stats: &PresentationStats) -> Self {
        self.display = DisplayTiming::from_presentation(stats);
        self
    }

    pub fn overlay(&self) -> OverlayText {
        let ms = |time: Duration| format!("{:.1}", millis(time));
        let mut lines = vec![
            overlay_row(
                "FPS",
                &format!("{:.0}", self.presented_fps),
                &format!("RECV {:.0}", self.received_fps),
            ),
            overlay_row("LOST", &format!("{:.1}", self.frames_lost), "/S"),
        ];

        if let Some(end_to_end) = self.end_to_end {
            lines.push(overlay_row("E2E", &ms(end_to_end.present), "MS"));
            lines.extend(
                end_to_end
                    .receive
                    .map(|time| overlay_row(" RX", &ms(time), "MS")),
            );
            lines.extend(
                end_to_end
                    .decode
                    .map(|time| overlay_row(" DEC", &ms(time), "MS")),
            );
        }
        if let Some(display) = self.display {
            lines.push(overlay_row(
                "DISPLAY",
                &format!("+{}", ms(display.submit_to_display)),
                &format!("MS {}", display.rate_label()),
            ));
            lines.extend(
                display
                    .capture_to_display
                    .map(|time| overlay_row(" E2E", &ms(time), "MS")),
            );
        }
        if let Some(latency) = self.latency {
            lines.push(overlay_row(
                "CLIENT",
                &ms(latency.total()),
                &format!("MS PEAK {}", ms(latency.peak)),
            ));
        }
        if let Some(report) = self.quality {
            lines.push(
                overlay_row(
                    "PSNR",
                    &decibels_text(report.psnr_overall),
                    &format!("DB XPSNR Y {}", decibels_text(report.xpsnr.luma)),
                )
                .to_uppercase(),
            );
        }

        OverlayText::new(lines)
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

const HOST_JSON_EVENT: &str = "host_stats";

#[derive(Debug, Clone, Copy)]
pub struct HostJsonLog {
    interval_ms: u32,
    schedule: Option<StatsSchedule>,
}

impl HostJsonLog {
    pub fn new(network: &NetworkConfig) -> Option<Self> {
        network.stats_json.then_some(Self {
            interval_ms: network.stats_log_interval_ms,
            schedule: None,
        })
    }

    pub fn admitted(&mut self, now: Instant) {
        self.schedule = Some(StatsSchedule::new(self.interval_ms, now));
    }

    pub fn observe(&mut self, stats: Option<&HostStats>, now: Instant) -> Option<String> {
        let stats = stats?;
        let due = self.schedule.as_mut()?.take_due(now);

        due.then(|| host_line(StatsPhase::Interval, stats))
    }

    pub fn left(&mut self, ended: Option<&HostStats>) -> Option<String> {
        self.schedule.take()?;

        ended.map(|stats| host_line(StatsPhase::Final, stats))
    }
}

fn host_line(phase: StatsPhase, stats: &HostStats) -> String {
    host_stats_json_line(HOST_JSON_EVENT, phase, unix_millis_now(), stats)
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

pub fn per_second(delta: u64, seconds: f64) -> f64 {
    to_f64(delta) / seconds
}

pub fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub fn millis_text(duration: Duration) -> String {
    format!("{:.1} ms", millis(duration))
}

pub fn fps_text(fps: f64) -> String {
    format!("{fps:.1} fps")
}

pub fn rate_text(per_second: f64) -> String {
    format!("{per_second:.1}/s")
}

pub fn bitrate_text(kbps: u32) -> String {
    format!("{:.1} Mbit/s", f64::from(kbps) / 1_000.0)
}

const OVERLAY_LABEL_CELLS: usize = 8;
const OVERLAY_VALUE_CELLS: usize = 6;

fn overlay_row(label: &str, value: &str, unit: &str) -> String {
    format!("{label:<OVERLAY_LABEL_CELLS$}{value:>OVERLAY_VALUE_CELLS$} {unit}")
        .trim_end()
        .to_owned()
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

    #[test]
    fn the_host_log_writes_its_own_lines_only_while_a_client_streams() {
        let start = Instant::now();
        let network = NetworkConfig {
            stats_json: true,
            stats_log_interval_ms: 1_000,
            ..NetworkConfig::default()
        };
        let stats = HostStats {
            frames_sent: 42,
            ..HostStats::default()
        };
        let mut log = HostJsonLog::new(&network).unwrap();

        let before = log.observe(Some(&stats), start + Duration::from_secs(5));
        log.admitted(start);
        let early = log.observe(Some(&stats), start + Duration::from_millis(999));
        let due = log.observe(Some(&stats), start + Duration::from_millis(1_000));
        let last = log.left(Some(&stats));
        let after = log.left(Some(&stats));

        assert!(HostJsonLog::new(&NetworkConfig::default()).is_none());
        assert_eq!(before, None);
        assert_eq!(early, None);
        let due = due.unwrap();
        assert!(due.starts_with(r#"{"event":"host_stats","phase":"interval","#));
        assert!(due.contains(r#""frames_sent":42"#));
        assert!(last.unwrap().contains(r#""phase":"final""#));
        assert_eq!(after, None);
    }

    fn quality_report() -> QualityReport {
        QualityReport {
            frame: 90,
            psnr: rusplay_client::QualityPlanes {
                luma: QualityDecibels(4_123),
                cb: QualityDecibels(4_400),
                cr: QualityDecibels::LOSSLESS,
            },
            psnr_overall: QualityDecibels(4_200),
            xpsnr: rusplay_client::QualityPlanes {
                luma: QualityDecibels(3_950),
                cb: QualityDecibels(4_300),
                cr: QualityDecibels(4_310),
            },
        }
    }

    #[test]
    fn quality_rows_say_the_state_and_show_a_report_with_its_age() {
        let off = quality_rows(&ClientStats::default());
        let refused = quality_rows(&ClientStats {
            quality: QualityState::Unavailable(QualityUnavailableReason::HostDisabled),
            ..ClientStats::default()
        });
        let reported = quality_rows(&ClientStats {
            quality: QualityState::Reported,
            quality_report: Some(quality_report()),
            quality_age: Some(Duration::from_millis(1_250)),
            ..ClientStats::default()
        });

        assert_eq!(off, [("Status", "Off".to_owned())]);
        assert_eq!(refused[0].1, "The host has sampling off");
        assert_eq!(
            reported[1],
            ("PSNR Y / U / V", "41.23 / 44.00 / lossless dB".to_owned())
        );
        assert_eq!(reported[2], ("PSNR overall", "42.00 dB".to_owned()));
        assert_eq!(reported[4], ("Sampled frame", "90".to_owned()));
        assert_eq!(reported[5], ("Sample age", "1.2 s".to_owned()));
    }

    #[test]
    fn the_overlay_gets_a_quality_line_while_measuring() {
        let start = Instant::now();
        let mut current = reading(start + SAMPLE_WINDOW);
        current.stats.quality = QualityState::Reported;
        current.stats.quality_report = Some(quality_report());

        let rates = StreamRates::between(&reading(start), &current, 0).unwrap();
        let quiet =
            StreamRates::between(&reading(start), &reading(start + SAMPLE_WINDOW), 0).unwrap();

        assert!(
            rates
                .overlay()
                .lines()
                .iter()
                .any(|line| line == "PSNR     42.00 DB XPSNR Y 39.50")
        );
        assert!(quiet.quality.is_none());
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
    }

    #[test]
    fn without_a_clock_offset_there_is_no_end_to_end_figure() {
        let start = Instant::now();
        let rates =
            StreamRates::between(&reading(start), &reading(start + Duration::from_secs(1)), 0)
                .unwrap();

        assert_eq!(rates.end_to_end, None);
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
                .any(|line| line == "DISPLAY   +4.2 MS 60 HZ")
        );
        assert!(
            rates
                .overlay()
                .lines()
                .iter()
                .any(|line| line == " E2E      21.0 MS")
        );
        assert!(
            variable
                .overlay()
                .lines()
                .iter()
                .any(|line| line == "DISPLAY   +4.2 MS VRR")
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
                "FPS         60 RECV 60",
                "LOST       0.0 /S",
                "E2E       12.3 MS",
                " RX        3.1 MS",
                " DEC       9.0 MS",
                "CLIENT     2.5 MS PEAK 6.5",
            ]
        );
    }
}
