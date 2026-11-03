use std::time::Duration;

use crate::{
    stats::{Counter, Gauge},
    window::pacing::{Feedback, PresentationFlags, Presented},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationStats {
    pub feedback: bool,
    pub frames_presented: u64,
    pub frames_discarded: u64,
    pub frames_coalesced: u64,
    pub display_latency: Option<Duration>,
    pub capture_to_display: Option<Duration>,
    pub refresh: Option<Duration>,
    pub margin: Option<Duration>,
    pub flags: PresentationFlags,
}

#[derive(Debug, Default)]
pub(crate) struct PresentationCounters {
    feedback: Counter,
    presented: Counter,
    discarded: Counter,
    coalesced: Counter,
    display_latency: Gauge,
    capture_to_display: Gauge,
    refresh_nanos: Counter,
    margin_nanos: Counter,
    flags: Counter,
}

impl PresentationCounters {
    pub(crate) fn set_feedback(&self, available: bool) {
        self.feedback.set(u64::from(available));
    }

    pub(crate) fn coalesced(&self, count: u64) {
        self.coalesced.add(count);
    }

    pub(crate) fn set_margin(&self, margin: Duration) {
        self.margin_nanos.set(nanos(margin));
    }

    pub(crate) fn record(&self, feedback: &Feedback) {
        match feedback {
            Feedback::Presented(presented) => self.presented(presented),
            Feedback::Discarded(_) => self.discarded.increment(),
        }
    }

    fn presented(&self, presented: &Presented) {
        self.presented.increment();
        self.display_latency.record(presented.display_latency());
        if let Some(latency) = presented.capture_to_display() {
            self.capture_to_display.record(latency);
        }
        self.refresh_nanos
            .set(presented.refresh.map_or(0, |refresh| nanos(refresh).max(1)));
        self.flags.set(u64::from(presented.flags.bits()));
    }

    pub(crate) fn snapshot(&self) -> PresentationStats {
        let duration = |value: u64| (value > 0).then(|| Duration::from_nanos(value));

        PresentationStats {
            feedback: self.feedback.get() != 0,
            frames_presented: self.presented.get(),
            frames_discarded: self.discarded.get(),
            frames_coalesced: self.coalesced.get(),
            display_latency: self.display_latency.get(),
            capture_to_display: self.capture_to_display.get(),
            refresh: duration(self.refresh_nanos.get()),
            margin: duration(self.margin_nanos.get()),
            flags: PresentationFlags::from_bits(
                u32::try_from(self.flags.get()).unwrap_or_default(),
            ),
        }
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::window::pacing::Submission;

    #[test]
    fn feedback_fills_the_presentation_statistics() {
        let counters = PresentationCounters::default();
        let submitted_at = Instant::now();
        let submission = Submission {
            submitted_at,
            captured_at: submitted_at.checked_sub(Duration::from_millis(20)),
            target: None,
        };

        counters.set_feedback(true);
        counters.coalesced(2);
        counters.record(&Feedback::Discarded(submission));
        counters.record(&Feedback::Presented(Presented {
            submission,
            shown_at: submitted_at + Duration::from_millis(5),
            refresh: Some(Duration::from_micros(6_944)),
            flags: PresentationFlags::from_bits(
                PresentationFlags::VSYNC | PresentationFlags::ZERO_COPY,
            ),
        }));
        let stats = counters.snapshot();

        assert!(stats.feedback);
        assert_eq!(stats.frames_presented, 1);
        assert_eq!(stats.frames_discarded, 1);
        assert_eq!(stats.frames_coalesced, 2);
        assert_eq!(stats.display_latency, Some(Duration::from_millis(5)));
        assert_eq!(stats.capture_to_display, Some(Duration::from_millis(25)));
        assert_eq!(stats.refresh, Some(Duration::from_micros(6_944)));
        assert!(stats.flags.vsync() && stats.flags.zero_copy());
        assert!(!stats.flags.hw_clock() && !stats.flags.hw_completion());
    }

    #[test]
    fn a_variable_refresh_reports_no_refresh_interval() {
        let counters = PresentationCounters::default();
        let at = Instant::now();

        counters.record(&Feedback::Presented(Presented {
            submission: Submission {
                submitted_at: at,
                captured_at: None,
                target: None,
            },
            shown_at: at,
            refresh: None,
            flags: PresentationFlags::default(),
        }));
        let stats = counters.snapshot();

        assert_eq!(stats.refresh, None);
        assert_eq!(stats.capture_to_display, None);
        assert_eq!(stats.frames_presented, 1);
    }
}
