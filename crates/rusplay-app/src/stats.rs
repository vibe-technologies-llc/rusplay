use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use rusplay_client::{ClientStats, ColorVolume, FrameSink, PresentedFrame};

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
        })
    }

    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{:.0} fps", self.presented_fps)];

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
        };

        assert_eq!(
            rates.summary(),
            "60 fps · 2.5 ms client (decode 2.0, present 0.5, peak 6.5) · received 60 fps · lost 0.0/s · recovered 1.5/s · keyframe requests 2.0/s"
        );
    }
}
