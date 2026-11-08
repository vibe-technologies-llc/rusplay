use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rusplay_protocol::{QualityReport, QualityUnavailableReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityState {
    #[default]
    Off,
    Waiting,
    Reported,
    Unavailable(QualityUnavailableReason),
}

impl QualityState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Waiting => "waiting",
            Self::Reported => "reported",
            Self::Unavailable(QualityUnavailableReason::EncoderUnsupported) => {
                "encoder-unsupported"
            }
            Self::Unavailable(QualityUnavailableReason::HostDisabled) => "host-disabled",
            Self::Unavailable(QualityUnavailableReason::MeasurementFailed) => "measurement-failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct QualitySnapshot {
    pub state: QualityState,
    pub report: Option<QualityReport>,
    pub age: Option<Duration>,
}

#[derive(Debug, Default)]
struct LinkState {
    wanted: bool,
    state: QualityState,
    latest: Option<(QualityReport, Instant)>,
}

#[derive(Debug, Default)]
pub(crate) struct QualityLink {
    inner: Mutex<LinkState>,
}

impl QualityLink {
    pub(crate) fn want(&self, enabled: bool) {
        let mut inner = self.inner.lock();
        inner.wanted = enabled;
        if !enabled {
            inner.state = QualityState::Off;
            inner.latest = None;
        } else if inner.state == QualityState::Off {
            inner.state = QualityState::Waiting;
        }
    }

    pub(crate) fn wanted(&self) -> bool {
        self.inner.lock().wanted
    }

    pub(crate) fn report(&self, report: QualityReport, now: Instant) {
        let mut inner = self.inner.lock();
        if inner.wanted {
            inner.state = QualityState::Reported;
            inner.latest = Some((report, now));
        }
    }

    pub(crate) fn unavailable(&self, reason: QualityUnavailableReason) {
        let mut inner = self.inner.lock();
        if inner.wanted {
            inner.state = QualityState::Unavailable(reason);
        }
    }

    pub(crate) fn snapshot(&self, now: Instant) -> QualitySnapshot {
        let inner = self.inner.lock();

        QualitySnapshot {
            state: inner.state,
            report: inner.latest.map(|(report, _)| report),
            age: inner
                .latest
                .map(|(_, received)| now.saturating_duration_since(received)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlight {
    id: u32,
    enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct QualityRequester {
    told: bool,
    in_flight: Option<InFlight>,
}

impl QualityRequester {
    pub(crate) fn ready(&self, wanted: bool) -> Option<bool> {
        (self.in_flight.is_none() && self.told != wanted).then_some(wanted)
    }

    pub(crate) fn sent(&mut self, enabled: bool, id: u32) {
        self.in_flight = Some(InFlight { id, enabled });
    }

    pub(crate) fn acked(&mut self, id: u32) {
        if let Some(in_flight) = self.in_flight.filter(|in_flight| in_flight.id == id) {
            self.told = in_flight.enabled;
            self.in_flight = None;
        }
    }

    pub(crate) fn undeliverable(&mut self, id: u32) {
        if self.in_flight.is_some_and(|in_flight| in_flight.id == id) {
            self.in_flight = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use rusplay_protocol::{QualityDecibels, QualityPlanes};

    use super::*;

    fn report(frame: u32) -> QualityReport {
        let planes = QualityPlanes {
            luma: QualityDecibels(4_000),
            cb: QualityDecibels(4_100),
            cr: QualityDecibels(4_200),
        };

        QualityReport {
            frame,
            psnr: planes,
            psnr_overall: QualityDecibels(4_050),
            xpsnr: planes,
        }
    }

    #[test]
    fn nothing_is_sent_while_the_host_already_has_the_wanted_state() {
        let requester = QualityRequester::default();

        assert_eq!(requester.ready(false), None);
        assert_eq!(requester.ready(true), Some(true));
    }

    #[test]
    fn one_request_is_in_flight_and_the_newest_wish_follows_the_ack() {
        let mut requester = QualityRequester::default();

        requester.sent(true, 7);
        let while_in_flight = requester.ready(false);
        requester.acked(7);
        let after = requester.ready(false);
        requester.sent(false, 8);
        requester.acked(8);

        assert_eq!(while_in_flight, None);
        assert_eq!(after, Some(false));
        assert_eq!(requester.ready(false), None);
    }

    #[test]
    fn an_undeliverable_request_is_sent_again() {
        let mut requester = QualityRequester::default();

        requester.sent(true, 3);
        requester.acked(2);
        requester.undeliverable(3);

        assert_eq!(requester.ready(true), Some(true));
    }

    #[test]
    fn the_link_waits_then_reports_with_the_age_and_forgets_when_turned_off() {
        let link = QualityLink::default();
        let now = Instant::now();

        let off = link.snapshot(now);
        link.want(true);
        let waiting = link.snapshot(now);
        link.report(report(5), now);
        let reported = link.snapshot(now + Duration::from_millis(300));
        link.want(false);
        link.report(report(6), now);
        let turned_off = link.snapshot(now);

        assert_eq!(off.state, QualityState::Off);
        assert_eq!(waiting.state, QualityState::Waiting);
        assert_eq!(reported.state, QualityState::Reported);
        assert_eq!(reported.report, Some(report(5)));
        assert_eq!(reported.age, Some(Duration::from_millis(300)));
        assert_eq!(turned_off, QualitySnapshot::default());
    }

    #[test]
    fn an_unavailable_answer_keeps_the_last_report() {
        let link = QualityLink::default();
        let now = Instant::now();

        link.want(true);
        link.report(report(1), now);
        link.unavailable(QualityUnavailableReason::MeasurementFailed);
        let snapshot = link.snapshot(now);

        assert_eq!(
            snapshot.state,
            QualityState::Unavailable(QualityUnavailableReason::MeasurementFailed)
        );
        assert_eq!(snapshot.report, Some(report(1)));
        assert_eq!(snapshot.state.name(), "measurement-failed");
    }
}
