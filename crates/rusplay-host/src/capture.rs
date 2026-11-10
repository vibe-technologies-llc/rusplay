use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use rusplay_capture::Frame;
use rusplay_core::FrameRate;

use crate::{CaptureFeed, HostError, shared::Shared};

const CAPTURE_POLL: Duration = Duration::from_millis(20);
const ARRIVAL_HISTORY: usize = 8;
const SUCCESSOR_GRACE_SHARE: u32 = 8;

#[derive(Debug)]
pub(crate) struct Captured {
    pub frame: Frame,
    pub captured_micros: u64,
}

pub(crate) fn run(
    feed: &CaptureFeed,
    shared: &Arc<Shared>,
    frame_rate: FrameRate,
) -> Result<(), HostError> {
    let result = capture(feed, shared, frame_rate);
    shared.stop();

    result
}

fn capture(feed: &CaptureFeed, shared: &Shared, frame_rate: FrameRate) -> Result<(), HostError> {
    let mut source = feed.source();
    let mut cadence = Cadence::default();

    if let Some(frame) = feed.take_retained() {
        cadence.admit(Instant::now(), frame_rate.frame_interval());
        offer(feed, shared, frame);
    }

    let mut pending = None;
    while !shared.stopped() {
        let wait = if pending.is_some() {
            cadence.wait(Instant::now()).min(CAPTURE_POLL)
        } else {
            CAPTURE_POLL
        };
        if let Some(frame) = source.next_frame(wait)? {
            shared.counters.frames_captured.increment();
            let captured = Captured {
                frame,
                captured_micros: shared.clock.micros(),
            };
            cadence.arrive(Instant::now());
            if pending.replace(captured).is_some() {
                shared.counters.frames_superseded.increment();
            }
        }
        let interval = shared.frame_rate(frame_rate).frame_interval();
        if pending.is_some() && cadence.admit(Instant::now(), interval) {
            let captured = pending.take().unwrap();
            feed.observe(&captured.frame);
            if let Ok(Some(_)) = shared.mailbox.put(captured) {
                shared.counters.frames_superseded.increment();
            }
        }
    }

    Ok(())
}

fn offer(feed: &CaptureFeed, shared: &Shared, frame: Frame) {
    feed.observe(&frame);
    let captured = Captured {
        frame,
        captured_micros: shared.clock.micros(),
    };
    if let Ok(Some(_)) = shared.mailbox.put(captured) {
        shared.counters.frames_superseded.increment();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spacing {
    shortest: Duration,
    longest: Duration,
}

#[derive(Debug, Default)]
struct Arrivals {
    latest: Option<Instant>,
    gaps: [Option<Duration>; ARRIVAL_HISTORY],
    cursor: usize,
}

impl Arrivals {
    fn record(&mut self, now: Instant) {
        if let Some(previous) = self.latest.replace(now) {
            self.gaps[self.cursor] = Some(now.saturating_duration_since(previous));
            self.cursor = (self.cursor + 1) % ARRIVAL_HISTORY;
        }
    }

    fn spacing(&self, interval: Duration) -> Spacing {
        let gaps = || self.gaps.iter().flatten().copied();

        Spacing {
            shortest: gaps().min().unwrap_or(interval).min(interval),
            longest: gaps().max().unwrap_or(interval),
        }
    }
}

#[derive(Debug, Default)]
struct Cadence {
    slot: Option<Instant>,
    interval: Option<Duration>,
    arrivals: Arrivals,
}

impl Cadence {
    fn arrive(&mut self, now: Instant) {
        self.arrivals.record(now);
    }

    fn admit(&mut self, now: Instant, interval: Option<Duration>) -> bool {
        if self.interval != interval {
            self.slot = None;
            self.interval = interval;
        }
        let Some(interval) = interval.filter(|interval| !interval.is_zero()) else {
            return true;
        };
        let Some(slot) = self.slot else {
            self.slot = Some(now + interval);
            return true;
        };
        if now < self.release_time(slot, interval) {
            return false;
        }

        self.slot = Some(if now.saturating_duration_since(slot) < interval {
            slot + interval
        } else {
            now + interval
        });
        true
    }

    fn wait(&self, now: Instant) -> Duration {
        let slotted = self
            .slot
            .zip(self.interval.filter(|interval| !interval.is_zero()));

        slotted.map_or(Duration::ZERO, |(slot, interval)| {
            self.release_time(slot, interval)
                .saturating_duration_since(now)
        })
    }

    fn release_time(&self, slot: Instant, interval: Duration) -> Instant {
        let Some(arrival) = self.arrivals.latest else {
            return slot;
        };
        let spacing = self.arrivals.spacing(interval);
        let successor_after_slot = arrival + spacing.shortest >= slot;
        let successor_before_slot = arrival + spacing.longest < slot;

        if successor_after_slot {
            arrival
        } else if successor_before_slot {
            slot
        } else {
            slot + interval / SUCCESSOR_GRACE_SHARE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIXTY_HZ: Duration = Duration::from_nanos(16_666_667);

    #[derive(Debug, Clone, Copy)]
    struct Release {
        arrived: Duration,
        released: Duration,
    }

    impl Release {
        fn held(self) -> Duration {
            self.released.saturating_sub(self.arrived)
        }
    }

    struct Simulation {
        start: Instant,
        cadence: Cadence,
        interval: Option<Duration>,
        pending: Option<Duration>,
        clock: Duration,
        released: Vec<Release>,
        superseded: usize,
    }

    impl Simulation {
        fn new(interval: Duration) -> Self {
            Self {
                start: Instant::now(),
                cadence: Cadence::default(),
                interval: Some(interval),
                pending: None,
                clock: Duration::ZERO,
                released: Vec::new(),
                superseded: 0,
            }
        }

        fn after_a_retained_frame(interval: Duration) -> Self {
            let mut simulation = Self::new(interval);
            let start = simulation.start;

            assert!(simulation.cadence.admit(start, simulation.interval));

            simulation
        }

        fn release_pending_before(&mut self, limit: Duration) {
            let Some(arrived) = self.pending else {
                return;
            };
            let due = self.clock + self.cadence.wait(self.start + self.clock);
            if due > limit {
                return;
            }

            assert!(self.cadence.admit(self.start + due, self.interval));

            self.pending = None;
            self.clock = due;
            self.released.push(Release {
                arrived,
                released: due,
            });
        }

        fn arrive(&mut self, at: Duration) {
            self.release_pending_before(at);
            self.clock = at;
            self.cadence.arrive(self.start + at);
            if self.pending.replace(at).is_some() {
                self.superseded += 1;
            }
            if self.cadence.admit(self.start + at, self.interval) {
                self.pending = None;
                self.released.push(Release {
                    arrived: at,
                    released: at,
                });
            }
        }

        fn run(mut self, arrivals: impl IntoIterator<Item = Duration>) -> Self {
            for at in arrivals {
                self.arrive(at);
            }
            self.release_pending_before(Duration::MAX);

            self
        }

        fn longest_hold(&self) -> Duration {
            self.released
                .iter()
                .map(|release| release.held())
                .max()
                .unwrap_or_default()
        }

        fn held_frames(&self) -> usize {
            self.released
                .iter()
                .filter(|release| !release.held().is_zero())
                .count()
        }
    }

    fn every(period: Duration, phase: Duration, frames: u32) -> impl Iterator<Item = Duration> {
        (0..frames).map(move |frame| phase + period * frame)
    }

    fn slots_within(span: Duration) -> usize {
        usize::try_from(span.as_nanos() / SIXTY_HZ.as_nanos()).unwrap()
    }

    fn jitter(frame: u32, spread: Duration) -> Duration {
        let hash = frame.wrapping_mul(2_654_435_761).rotate_left(13) % 1_000;

        spread * hash / 1_000
    }

    #[test]
    fn a_240_hz_source_is_limited_to_60_hz_before_encoding() {
        let simulation = Simulation::new(SIXTY_HZ).run(every(
            Duration::from_nanos(4_166_667),
            Duration::ZERO,
            240,
        ));

        assert!((60..=61).contains(&simulation.released.len()));
        assert_eq!(simulation.longest_hold(), Duration::ZERO);
    }

    #[test]
    fn a_source_at_the_capped_rate_is_never_held_whatever_its_phase() {
        for phase_ms in [0, 1, 4, 8, 12, 15, 16] {
            let phase = Duration::from_millis(phase_ms);
            let simulation =
                Simulation::after_a_retained_frame(SIXTY_HZ).run(every(SIXTY_HZ, phase, 600));

            assert_eq!(simulation.released.len(), 600, "phase {phase_ms} ms");
            assert_eq!(
                simulation.longest_hold(),
                Duration::ZERO,
                "phase {phase_ms} ms"
            );
        }
    }

    #[test]
    fn a_source_slightly_faster_than_the_cap_drops_a_frame_instead_of_holding_every_one() {
        let period = Duration::from_micros(16_600);
        let frames = 3_000;
        let simulation = Simulation::after_a_retained_frame(SIXTY_HZ).run(every(
            period,
            Duration::from_millis(3),
            frames,
        ));
        let surplus = usize::try_from(frames).unwrap() - slots_within(period * frames);

        assert!(simulation.superseded <= surplus + 1);
        assert!(simulation.released.len() >= frames as usize - surplus - 1);
        assert!(simulation.held_frames() <= surplus + 1);
    }

    #[test]
    fn a_144_hz_source_capped_to_60_hz_goes_out_on_arrival() {
        let simulation = Simulation::new(SIXTY_HZ).run(every(
            Duration::from_nanos(6_944_444),
            Duration::ZERO,
            1_440,
        ));

        let (last, earlier) = simulation.released.split_last().unwrap();

        assert!((599..=601).contains(&simulation.released.len()));
        assert!(earlier.iter().all(|release| release.held().is_zero()));
        assert!(last.held() < SIXTY_HZ);
    }

    #[test]
    fn a_jittering_source_at_the_capped_rate_is_rarely_held() {
        let spread = Duration::from_millis(2);
        let arrivals = (0..6_000).map(|frame| SIXTY_HZ * frame + jitter(frame, spread));
        let simulation = Simulation::after_a_retained_frame(SIXTY_HZ).run(arrivals);
        let held: Duration = simulation
            .released
            .iter()
            .map(|release| release.held())
            .sum();

        assert!(simulation.released.len() >= 5_990);
        assert!(held / 6_000 < Duration::from_micros(50));
        assert!(simulation.longest_hold() <= SIXTY_HZ + SIXTY_HZ / SUCCESSOR_GRACE_SHARE);
    }

    #[test]
    fn the_last_frame_of_a_burst_is_released_soon_after_its_slot() {
        let burst = every(Duration::from_millis(3), Duration::ZERO, 4);
        let simulation = Simulation::new(SIXTY_HZ).run(burst);
        let last = simulation.released.last().unwrap();

        assert_eq!(last.arrived, Duration::from_millis(9));
        assert!(last.released <= SIXTY_HZ + SIXTY_HZ / SUCCESSOR_GRACE_SHARE);
        assert!(last.released >= SIXTY_HZ);
    }

    #[test]
    fn rate_changes_take_effect_immediately_and_stalls_do_not_burst() {
        let start = Instant::now();
        let mut cadence = Cadence::default();
        assert!(cadence.admit(start, Some(Duration::from_millis(20))));
        assert!(!cadence.admit(
            start + Duration::from_millis(5),
            Some(Duration::from_millis(20))
        ));
        assert!(cadence.admit(
            start + Duration::from_millis(5),
            Some(Duration::from_millis(10))
        ));
        assert!(cadence.admit(
            start + Duration::from_secs(1),
            Some(Duration::from_millis(10))
        ));
        assert!(!cadence.admit(
            start + Duration::from_secs(1),
            Some(Duration::from_millis(10))
        ));
    }

    #[test]
    fn irregular_sources_keep_the_capped_rate_and_a_bounded_hold() {
        let frames = 20_000;
        for (period_nanos, spread_micros) in [
            (16_660_000, 1_000),
            (16_670_000, 1_500),
            (11_111_111, 6_000),
            (6_944_444, 1_500),
        ] {
            let period = Duration::from_nanos(period_nanos);
            let spread = Duration::from_micros(spread_micros);
            let arrivals = (0..frames).map(|frame| period * frame + jitter(frame, spread));
            let simulation = Simulation::after_a_retained_frame(SIXTY_HZ).run(arrivals);
            let slots = slots_within(period * frames);
            let released = simulation.released.len();

            assert!(released <= slots + 2, "{period:?}: {released} of {slots}");
            assert!(
                released + 2 >= slots.min(frames as usize),
                "{period:?}: {released} of {slots}"
            );
            assert!(simulation.longest_hold() <= SIXTY_HZ + SIXTY_HZ / SUCCESSOR_GRACE_SHARE);
        }
    }
}
