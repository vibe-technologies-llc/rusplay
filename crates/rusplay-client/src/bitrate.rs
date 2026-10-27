use std::time::{Duration, Instant};

use rusplay_config::NetworkConfig;

const WINDOW_RTTS: u32 = 8;
const HOLD_RTTS: u32 = 2;
const HOLD_WINDOW_FRACTION: u32 = 4;
const PERCENT: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BitrateSettings {
    pub ceiling_kbps: u32,
    pub floor_kbps: u32,
    pub step_kbps: u32,
    pub backoff_percent: u8,
    pub window: Duration,
}

impl BitrateSettings {
    pub(crate) fn from_config(network: &NetworkConfig, ceiling_kbps: u32) -> Option<Self> {
        network.adaptive_bitrate.then(|| Self {
            ceiling_kbps,
            floor_kbps: network.adaptive_bitrate_floor_kbps.min(ceiling_kbps),
            step_kbps: network.adaptive_bitrate_step_kbps,
            backoff_percent: network.adaptive_bitrate_backoff_percent,
            window: Duration::from_millis(u64::from(network.adaptive_bitrate_window_ms)),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BitrateController {
    settings: BitrateSettings,
    target_kbps: u32,
    clean_since: Instant,
    last_decrease: Option<Instant>,
}

impl BitrateController {
    pub(crate) fn new(settings: BitrateSettings, now: Instant) -> Self {
        Self {
            settings,
            target_kbps: settings.ceiling_kbps,
            clean_since: now,
            last_decrease: None,
        }
    }

    pub(crate) fn target_kbps(&self) -> u32 {
        self.target_kbps
    }

    pub(crate) fn congestion(&mut self, now: Instant, rtt: Option<Duration>) -> Option<u32> {
        self.clean_since = now;

        let holding = self
            .last_decrease
            .is_some_and(|last| now.saturating_duration_since(last) < self.hold(rtt));
        if holding {
            return None;
        }

        let cut = u64::from(self.target_kbps) * u64::from(self.settings.backoff_percent) / PERCENT;
        let reduced = u32::try_from(u64::from(self.target_kbps) - cut)
            .unwrap_or(self.target_kbps)
            .max(self.settings.floor_kbps);
        self.last_decrease = Some(now);

        self.change_to(reduced)
    }

    pub(crate) fn poll(&mut self, now: Instant, rtt: Option<Duration>) -> Option<u32> {
        let due = self.next_deadline(rtt)?;
        if now < due {
            return None;
        }

        self.clean_since = now;
        let raised = self
            .target_kbps
            .saturating_add(self.settings.step_kbps)
            .min(self.settings.ceiling_kbps);

        self.change_to(raised)
    }

    pub(crate) fn next_deadline(&self, rtt: Option<Duration>) -> Option<Instant> {
        (self.target_kbps < self.settings.ceiling_kbps).then(|| self.clean_since + self.window(rtt))
    }

    fn change_to(&mut self, kbps: u32) -> Option<u32> {
        (kbps != self.target_kbps).then(|| {
            self.target_kbps = kbps;
            kbps
        })
    }

    fn window(&self, rtt: Option<Duration>) -> Duration {
        rtt.map_or(self.settings.window, |rtt| {
            self.settings.window.max(rtt * WINDOW_RTTS)
        })
    }

    fn hold(&self, rtt: Option<Duration>) -> Duration {
        let share = self.settings.window / HOLD_WINDOW_FRACTION;

        rtt.map_or(share, |rtt| share.max(rtt * HOLD_RTTS))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HintOutbox {
    pending_kbps: Option<u32>,
    in_flight: Option<u32>,
}

impl HintOutbox {
    pub(crate) fn queue(&mut self, kbps: u32) {
        self.pending_kbps = Some(kbps);
    }

    pub(crate) fn ready(&self) -> Option<u32> {
        self.pending_kbps.filter(|_| self.in_flight.is_none())
    }

    pub(crate) fn sent(&mut self, id: u32) {
        self.pending_kbps = None;
        self.in_flight = Some(id);
    }

    pub(crate) fn acked(&mut self, id: u32) {
        if self.in_flight == Some(id) {
            self.in_flight = None;
        }
    }

    pub(crate) fn undeliverable(&mut self, id: u32, current_kbps: u32) {
        if self.in_flight == Some(id) {
            self.in_flight = None;
            self.pending_kbps.get_or_insert(current_kbps);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn settings() -> BitrateSettings {
        BitrateSettings {
            ceiling_kbps: 80_000,
            floor_kbps: 10_000,
            step_kbps: 5_000,
            backoff_percent: 25,
            window: Duration::from_secs(1),
        }
    }

    #[test]
    fn congestion_cuts_the_target_by_the_backoff_share() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);

        let hint = controller.congestion(start + 10 * MS, None);

        assert_eq!(hint, Some(60_000));
        assert_eq!(controller.target_kbps(), 60_000);
    }

    #[test]
    fn a_burst_of_loss_inside_the_hold_cuts_only_once() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);

        let first = controller.congestion(start, None);
        let burst = controller.congestion(start + 100 * MS, None);
        let tail = controller.congestion(start + 249 * MS, None);
        let next = controller.congestion(start + 250 * MS, None);

        assert_eq!(first, Some(60_000));
        assert_eq!(burst, None);
        assert_eq!(tail, None);
        assert_eq!(next, Some(45_000));
    }

    #[test]
    fn a_long_round_trip_stretches_the_hold() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);
        let rtt = Some(200 * MS);

        controller.congestion(start, rtt);
        let held = controller.congestion(start + 300 * MS, rtt);
        let released = controller.congestion(start + 400 * MS, rtt);

        assert_eq!(held, None);
        assert_eq!(released, Some(45_000));
    }

    #[test]
    fn repeated_congestion_stops_at_the_floor() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);

        let hints: Vec<Option<u32>> = (0..10)
            .map(|step| controller.congestion(start + step * Duration::from_secs(1), None))
            .collect();

        assert_eq!(
            hints,
            [
                Some(60_000),
                Some(45_000),
                Some(33_750),
                Some(25_313),
                Some(18_985),
                Some(14_239),
                Some(10_680),
                Some(10_000),
                None,
                None,
            ]
        );
        assert_eq!(controller.target_kbps(), 10_000);
    }

    #[test]
    fn a_clean_window_raises_the_target_by_one_step() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);
        controller.congestion(start, None);

        let early = controller.poll(start + 999 * MS, None);
        let first = controller.poll(start + 1_000 * MS, None);
        let same_window = controller.poll(start + 1_500 * MS, None);
        let second = controller.poll(start + 2_000 * MS, None);

        assert_eq!(early, None);
        assert_eq!(first, Some(65_000));
        assert_eq!(same_window, None);
        assert_eq!(second, Some(70_000));
    }

    #[test]
    fn loss_restarts_the_clean_window_even_while_holding() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);
        controller.congestion(start, None);

        controller.congestion(start + 200 * MS, None);
        let reset = controller.poll(start + 1_000 * MS, None);
        let raised = controller.poll(start + 1_200 * MS, None);

        assert_eq!(reset, None);
        assert_eq!(raised, Some(65_000));
    }

    #[test]
    fn the_increase_never_passes_the_ceiling() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);
        controller.congestion(start, None);

        let hints: Vec<Option<u32>> = (1..=6)
            .map(|second| controller.poll(start + second * Duration::from_secs(1), None))
            .collect();

        assert_eq!(
            hints,
            [
                Some(65_000),
                Some(70_000),
                Some(75_000),
                Some(80_000),
                None,
                None
            ]
        );
        assert_eq!(controller.next_deadline(None), None);
    }

    #[test]
    fn at_the_ceiling_nothing_is_due() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);

        assert_eq!(controller.next_deadline(None), None);
        assert_eq!(controller.poll(start + Duration::from_secs(60), None), None);
    }

    #[test]
    fn a_long_round_trip_stretches_the_window() {
        let start = Instant::now();
        let mut controller = BitrateController::new(settings(), start);
        let rtt = Some(250 * MS);
        controller.congestion(start, rtt);

        let deadline = controller.next_deadline(rtt);
        let early = controller.poll(start + 1_999 * MS, rtt);
        let raised = controller.poll(start + 2_000 * MS, rtt);

        assert_eq!(deadline, Some(start + 2_000 * MS));
        assert_eq!(early, None);
        assert_eq!(raised, Some(65_000));
    }

    #[test]
    fn a_floor_above_the_ceiling_is_lowered_to_it() {
        let network = NetworkConfig {
            adaptive_bitrate_floor_kbps: 50_000,
            ..NetworkConfig::default()
        };
        let disabled = NetworkConfig {
            adaptive_bitrate: false,
            ..NetworkConfig::default()
        };

        let settings = BitrateSettings::from_config(&network, 20_000).unwrap();

        assert_eq!(settings.floor_kbps, 20_000);
        assert_eq!(settings.ceiling_kbps, 20_000);
        assert_eq!(BitrateSettings::from_config(&disabled, 20_000), None);
    }

    #[test]
    fn the_outbox_sends_one_hint_at_a_time_and_keeps_only_the_newest() {
        let mut outbox = HintOutbox::default();

        outbox.queue(60_000);
        let first = outbox.ready();
        outbox.sent(7);
        outbox.queue(45_000);
        outbox.queue(33_750);
        let blocked = outbox.ready();
        outbox.acked(6);
        let still_blocked = outbox.ready();
        outbox.acked(7);
        let next = outbox.ready();

        assert_eq!(first, Some(60_000));
        assert_eq!(blocked, None);
        assert_eq!(still_blocked, None);
        assert_eq!(next, Some(33_750));
    }

    #[test]
    fn an_undeliverable_hint_is_replaced_by_the_current_target() {
        let mut outbox = HintOutbox::default();
        outbox.queue(60_000);
        outbox.sent(3);

        outbox.undeliverable(3, 65_000);

        assert_eq!(outbox.ready(), Some(65_000));
    }
}
