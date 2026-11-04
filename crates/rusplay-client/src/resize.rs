use std::time::{Duration, Instant};

use rusplay_protocol::ClientDisplay;

#[derive(Debug, Clone, Copy)]
struct Wanted {
    display: ClientDisplay,
    since: Instant,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DisplayAnnouncer {
    debounce: Duration,
    sent: ClientDisplay,
    acknowledged: ClientDisplay,
    wanted: Option<Wanted>,
    in_flight: Option<u32>,
}

impl DisplayAnnouncer {
    pub(crate) fn new(initial: ClientDisplay, debounce: Duration) -> Self {
        Self {
            debounce,
            sent: initial,
            acknowledged: initial,
            wanted: None,
            in_flight: None,
        }
    }

    pub(crate) fn request(&mut self, display: ClientDisplay, now: Instant) {
        if display == self.sent {
            self.wanted = None;
        } else if self.wanted.is_none_or(|wanted| wanted.display != display) {
            self.wanted = Some(Wanted {
                display,
                since: now,
            });
        }
    }

    pub(crate) fn ready(&self, now: Instant) -> Option<ClientDisplay> {
        let wanted = self.wanted?;

        (self.in_flight.is_none() && now.saturating_duration_since(wanted.since) >= self.debounce)
            .then_some(wanted.display)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.wanted
            .filter(|_| self.in_flight.is_none())
            .map(|wanted| wanted.since + self.debounce)
    }

    pub(crate) fn sent(&mut self, display: ClientDisplay, id: u32) {
        self.sent = display;
        self.in_flight = Some(id);
        if self.wanted.is_some_and(|wanted| wanted.display == display) {
            self.wanted = None;
        }
    }

    pub(crate) fn acked(&mut self, id: u32) {
        if self.in_flight == Some(id) {
            self.in_flight = None;
            self.acknowledged = self.sent;
        }
    }

    pub(crate) fn undeliverable(&mut self, id: u32, now: Instant) {
        if self.in_flight == Some(id) {
            self.in_flight = None;
            let lost = self.sent;
            self.sent = self.acknowledged;
            self.wanted.get_or_insert(Wanted {
                display: lost,
                since: now,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use rusplay_core::Resolution;
    use rusplay_protocol::DisplayVolume;

    use super::*;

    const DEBOUNCE: Duration = Duration::from_millis(200);
    const MS: Duration = Duration::from_millis(1);

    fn display(width: u32, height: u32) -> ClientDisplay {
        ClientDisplay::new(
            Resolution::new(width, height),
            60_000,
            DisplayVolume::srgb(),
        )
        .unwrap()
    }

    fn announcer() -> DisplayAnnouncer {
        DisplayAnnouncer::new(display(1280, 720), DEBOUNCE)
    }

    #[test]
    fn a_new_display_is_held_until_it_has_been_still_for_the_debounce() {
        let start = Instant::now();
        let mut announcer = announcer();

        announcer.request(display(1300, 720), start);

        assert_eq!(announcer.ready(start + 199 * MS), None);
        assert_eq!(announcer.ready(start + 200 * MS), Some(display(1300, 720)));
        assert_eq!(announcer.next_deadline(), Some(start + DEBOUNCE));
    }

    #[test]
    fn a_drag_restarts_the_debounce_with_every_new_size_and_sends_only_the_last() {
        let start = Instant::now();
        let mut announcer = announcer();

        for step in 0..50u32 {
            let now = start + 10 * MS * step;
            announcer.request(display(1300 + step, 720), now);
            assert_eq!(announcer.ready(now), None);
        }
        let last = start + 490 * MS;

        assert_eq!(announcer.ready(last + 199 * MS), None);
        assert_eq!(announcer.ready(last + 200 * MS), Some(display(1349, 720)));
    }

    #[test]
    fn asking_again_for_the_same_pending_size_does_not_restart_the_wait() {
        let start = Instant::now();
        let mut announcer = announcer();

        announcer.request(display(1300, 720), start);
        announcer.request(display(1300, 720), start + 150 * MS);

        assert_eq!(announcer.ready(start + 200 * MS), Some(display(1300, 720)));
    }

    #[test]
    fn returning_to_the_size_the_host_has_cancels_the_announcement() {
        let start = Instant::now();
        let mut announcer = announcer();

        announcer.request(display(1300, 720), start);
        announcer.request(display(1280, 720), start + 50 * MS);

        assert_eq!(announcer.ready(start + 10_000 * MS), None);
        assert_eq!(announcer.next_deadline(), None);
    }

    #[test]
    fn a_zero_debounce_announces_at_once() {
        let start = Instant::now();
        let mut announcer = DisplayAnnouncer::new(display(1280, 720), Duration::ZERO);

        announcer.request(display(1300, 720), start);

        assert_eq!(announcer.ready(start), Some(display(1300, 720)));
    }

    #[test]
    fn one_announcement_is_in_flight_at_a_time_and_the_newest_follows_its_acknowledgement() {
        let start = Instant::now();
        let mut announcer = announcer();

        announcer.request(display(1300, 720), start);
        announcer.sent(display(1300, 720), 7);
        announcer.request(display(1400, 800), start + 10 * MS);
        announcer.request(display(1500, 900), start + 20 * MS);
        let blocked = announcer.ready(start + 10_000 * MS);
        announcer.acked(6);
        let still_blocked = announcer.ready(start + 10_000 * MS);
        announcer.acked(7);
        let next = announcer.ready(start + 10_000 * MS);

        assert_eq!(blocked, None);
        assert_eq!(still_blocked, None);
        assert_eq!(next, Some(display(1500, 900)));
    }

    #[test]
    fn an_undeliverable_announcement_is_sent_again_unless_a_newer_one_waits() {
        let start = Instant::now();
        let mut announcer = announcer();
        announcer.request(display(1300, 720), start);
        announcer.sent(display(1300, 720), 3);

        announcer.undeliverable(3, start + 400 * MS);

        assert_eq!(announcer.ready(start + 400 * MS), None);
        assert_eq!(announcer.ready(start + 600 * MS), Some(display(1300, 720)));

        announcer.sent(display(1300, 720), 4);
        announcer.request(display(1400, 800), start + 700 * MS);
        announcer.undeliverable(4, start + 800 * MS);

        assert_eq!(announcer.ready(start + 900 * MS), Some(display(1400, 800)));
    }

    #[test]
    fn the_connect_announcement_blocks_updates_until_it_is_acknowledged() {
        let start = Instant::now();
        let mut announcer = announcer();
        announcer.sent(display(1280, 720), 1);

        announcer.request(display(1300, 720), start);

        assert_eq!(announcer.ready(start + 10_000 * MS), None);

        announcer.acked(1);

        assert_eq!(
            announcer.ready(start + 10_000 * MS),
            Some(display(1300, 720))
        );
    }
}
