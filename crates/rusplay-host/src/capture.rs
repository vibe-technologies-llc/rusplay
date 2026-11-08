use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use rusplay_capture::Frame;
use rusplay_core::FrameRate;

use crate::{CaptureFeed, HostError, shared::Shared};

const CAPTURE_POLL: Duration = Duration::from_millis(20);

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
            cadence.next.map_or(CAPTURE_POLL, |next| {
                next.saturating_duration_since(Instant::now())
                    .min(CAPTURE_POLL)
            })
        } else {
            CAPTURE_POLL
        };
        if let Some(frame) = source.next_frame(wait)? {
            shared.counters.frames_captured.increment();
            let captured = Captured {
                frame,
                captured_micros: shared.clock.micros(),
            };
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

#[derive(Default)]
struct Cadence {
    next: Option<Instant>,
    interval: Option<Duration>,
}

impl Cadence {
    fn admit(&mut self, now: Instant, interval: Option<Duration>) -> bool {
        if self.interval != interval {
            self.next = None;
            self.interval = interval;
        }
        let Some(interval) = interval.filter(|interval| !interval.is_zero()) else {
            return true;
        };
        if let Some(next) = self.next {
            if now < next {
                return false;
            }
            self.next = Some(if now.duration_since(next) < interval {
                next + interval
            } else {
                now + interval
            });
        } else {
            self.next = Some(now + interval);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_240_hz_source_is_limited_to_60_hz_before_encoding() {
        let start = Instant::now();
        let mut cadence = Cadence::default();
        let interval = FrameRate::integer(60).frame_interval();
        let accepted = (0..240)
            .filter(|tick| {
                cadence.admit(
                    start + Duration::from_nanos(*tick * 1_000_000_000 / 240),
                    interval,
                )
            })
            .count();
        assert_eq!(accepted, 60);
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
}
