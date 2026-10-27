use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, AtomicI64, Ordering},
    time::{Duration, Instant},
};

const WINDOW: usize = 8;
const WARMUP_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) struct HostClock {
    origin: Instant,
    offset_micros: AtomicI64,
    known: AtomicBool,
}

impl Default for HostClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            offset_micros: AtomicI64::new(0),
            known: AtomicBool::new(false),
        }
    }
}

impl HostClock {
    pub(crate) fn local_micros(&self, at: Instant) -> i64 {
        i64::try_from(at.saturating_duration_since(self.origin).as_micros()).unwrap_or(i64::MAX)
    }

    pub(crate) fn set_offset(&self, offset_micros: i64) {
        self.offset_micros.store(offset_micros, Ordering::Relaxed);
        self.known.store(true, Ordering::Release);
    }

    pub(crate) fn offset(&self) -> Option<i64> {
        self.known
            .load(Ordering::Acquire)
            .then(|| self.offset_micros.load(Ordering::Relaxed))
    }

    pub(crate) fn since_capture(&self, captured_micros: u64, at: Instant) -> Option<Duration> {
        let host_now = self.local_micros(at).saturating_add(self.offset()?);
        let captured = i64::try_from(captured_micros).ok()?;

        Some(Duration::from_micros(
            host_now.saturating_sub(captured).max(0).unsigned_abs(),
        ))
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    round_trip_micros: i64,
    offset_micros: i64,
}

#[derive(Debug)]
pub(crate) struct ClockSync {
    interval: Option<Duration>,
    samples: VecDeque<Sample>,
    next_ping: Instant,
}

impl ClockSync {
    pub(crate) fn new(interval_ms: u32, now: Instant) -> Self {
        Self {
            interval: (interval_ms > 0).then(|| Duration::from_millis(u64::from(interval_ms))),
            samples: VecDeque::with_capacity(WINDOW),
            next_ping: now,
        }
    }

    pub(crate) fn next_ping(&self) -> Option<Instant> {
        self.interval.map(|_| self.next_ping)
    }

    pub(crate) fn ping_due(&self, now: Instant) -> bool {
        self.interval.is_some() && now >= self.next_ping
    }

    pub(crate) fn pinged(&mut self, now: Instant) {
        let Some(interval) = self.interval else {
            return;
        };
        let wait = if self.samples.len() < WINDOW {
            WARMUP_INTERVAL.min(interval)
        } else {
            interval
        };
        self.next_ping = now + wait;
    }

    pub(crate) fn record(
        &mut self,
        sent_micros: i64,
        host_micros: i64,
        received_micros: i64,
    ) -> Option<i64> {
        let round_trip_micros = received_micros
            .checked_sub(sent_micros)
            .filter(|rtt| *rtt >= 0)?;
        let midpoint = sent_micros.saturating_add(round_trip_micros / 2);

        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            round_trip_micros,
            offset_micros: host_micros.saturating_sub(midpoint),
        });

        self.samples
            .iter()
            .min_by_key(|sample| sample.round_trip_micros)
            .map(|sample| sample.offset_micros)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_samples() -> i64 {
        i64::try_from(WINDOW).unwrap()
    }

    #[test]
    fn the_offset_is_the_host_time_minus_the_midpoint_of_the_exchange() {
        let mut sync = ClockSync::new(1_000, Instant::now());

        let offset = sync.record(1_000, 51_500, 2_000);

        assert_eq!(offset, Some(50_000));
    }

    #[test]
    fn the_sample_with_the_smallest_round_trip_wins() {
        let mut sync = ClockSync::new(1_000, Instant::now());

        sync.record(0, 10_000 + 5_000, 10_000);
        let best = sync.record(100_000, 100_000 + 20_000 + 500, 101_000);
        let later = sync.record(200_000, 200_000 + 20_000 + 9_000, 218_000);

        assert_eq!(best, Some(20_000));
        assert_eq!(later, Some(20_000));
    }

    #[test]
    fn old_samples_age_out_of_the_window() {
        let mut sync = ClockSync::new(1_000, Instant::now());
        sync.record(0, 90, 100);

        let mut latest = None;
        for step in 1..=window_samples() {
            let sent = step * 10_000;
            latest = sync.record(sent, sent + 1_000 + 500, sent + 1_000);
        }

        assert_eq!(latest, Some(1_000));
    }

    #[test]
    fn a_reply_that_arrives_before_it_was_sent_is_ignored() {
        let mut sync = ClockSync::new(1_000, Instant::now());

        assert_eq!(sync.record(500, 0, 100), None);
    }

    #[test]
    fn pings_come_quickly_until_the_window_fills_and_then_at_the_interval() {
        let start = Instant::now();
        let mut sync = ClockSync::new(1_000, start);

        assert!(sync.ping_due(start));
        sync.pinged(start);
        let warming = sync.next_ping().unwrap() - start;
        for index in 0..window_samples() {
            sync.record(index, index, index + 2);
        }
        sync.pinged(start);
        let settled = sync.next_ping().unwrap() - start;

        assert_eq!(warming, WARMUP_INTERVAL);
        assert_eq!(settled, Duration::from_secs(1));
    }

    #[test]
    fn a_zero_interval_turns_syncing_off() {
        let start = Instant::now();
        let mut sync = ClockSync::new(0, start);
        sync.pinged(start);

        assert!(!sync.ping_due(start + Duration::from_secs(60)));
        assert_eq!(sync.next_ping(), None);
    }

    #[test]
    fn latency_needs_an_offset_and_never_goes_negative() {
        let clock = HostClock::default();
        let later = Instant::now() + Duration::from_millis(40);
        let host_when_local_zero = 1_000_000;

        let unknown = clock.since_capture(0, later);
        clock.set_offset(host_when_local_zero);
        let behind = clock.since_capture(host_when_local_zero.cast_unsigned() + 10_000_000, later);
        let measured = clock
            .since_capture(host_when_local_zero.cast_unsigned(), later)
            .unwrap();

        assert_eq!(unknown, None);
        assert_eq!(behind, Some(Duration::ZERO));
        assert!(measured >= Duration::from_millis(40));
        assert!(measured < Duration::from_secs(5));
    }
}
