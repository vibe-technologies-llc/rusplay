use std::{
    fmt::{Display, Write},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsPhase {
    Interval,
    Final,
}

impl StatsPhase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Interval => "interval",
            Self::Final => "final",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonLine(String);

impl JsonLine {
    pub fn new(event: &str, phase: StatsPhase, unix_ms: u128) -> Self {
        Self(format!(
            r#"{{"event":"{event}","phase":"{}","unix_ms":{unix_ms}"#,
            phase.name()
        ))
    }

    #[must_use]
    pub fn number(mut self, name: &str, value: impl Display) -> Self {
        write!(self.0, ",\"{name}\":{value}").ok();
        self
    }

    #[must_use]
    pub fn optional(mut self, name: &str, value: Option<impl Display>) -> Self {
        match value {
            Some(value) => write!(self.0, ",\"{name}\":{value}"),
            None => write!(self.0, ",\"{name}\":null"),
        }
        .ok();
        self
    }

    pub fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }
}

pub fn unix_millis_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsSchedule {
    interval: Option<Duration>,
    next: Instant,
}

impl StatsSchedule {
    pub fn new(interval_ms: u32, now: Instant) -> Self {
        let interval = (interval_ms > 0).then(|| Duration::from_millis(u64::from(interval_ms)));

        Self {
            interval,
            next: now + interval.unwrap_or_default(),
        }
    }

    pub fn take_due(&mut self, now: Instant) -> bool {
        let Some(interval) = self.interval else {
            return false;
        };
        if now < self.next {
            return false;
        }

        self.next = now + interval;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_opens_with_the_event_phase_and_time_and_closes_as_one_object() {
        let line = JsonLine::new("stats", StatsPhase::Interval, 1_700_000_000_123)
            .number("frames", 600)
            .optional("rtt_us", Some(1_250))
            .optional("missing", None::<u32>)
            .finish();

        assert_eq!(
            line,
            r#"{"event":"stats","phase":"interval","unix_ms":1700000000123,"frames":600,"rtt_us":1250,"missing":null}"#
        );
    }

    #[test]
    fn the_final_phase_is_named() {
        let line = JsonLine::new("client_stats", StatsPhase::Final, 0).finish();

        assert_eq!(
            line,
            r#"{"event":"client_stats","phase":"final","unix_ms":0}"#
        );
    }

    #[test]
    fn zero_interval_never_comes_due() {
        let start = Instant::now();
        let mut schedule = StatsSchedule::new(0, start);

        assert!(!schedule.take_due(start + Duration::from_secs(3_600)));
    }

    #[test]
    fn interval_comes_due_once_per_period() {
        let start = Instant::now();
        let mut schedule = StatsSchedule::new(1_000, start);

        assert!(!schedule.take_due(start + Duration::from_millis(999)));
        assert!(schedule.take_due(start + Duration::from_millis(1_000)));
        assert!(!schedule.take_due(start + Duration::from_millis(1_500)));
        assert!(schedule.take_due(start + Duration::from_millis(2_000)));
    }
}
