use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use rusplay_config::{PresentPacing, VideoConfig};

const STALE_AFTER: Duration = Duration::from_secs(2);
const DELAY_WINDOW: usize = 120;
const MARGIN_STEP_DIVISOR: u32 = 4;
const MARGIN_RELAX_DIVISOR: u32 = 16;
pub(crate) const PACED_FRAMES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PacingOptions {
    pub(crate) mode: PresentPacing,
    pub(crate) margin: Duration,
    pub(crate) jitter: Duration,
}

impl From<&VideoConfig> for PacingOptions {
    fn from(video: &VideoConfig) -> Self {
        Self {
            mode: video.present_pacing,
            margin: Duration::from_micros(u64::from(video.present_margin_us)),
            jitter: Duration::from_micros(u64::from(video.present_jitter_us)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresentationFlags(u32);

impl PresentationFlags {
    pub(crate) const VSYNC: u32 = 0x1;
    pub(crate) const HW_CLOCK: u32 = 0x2;
    pub(crate) const HW_COMPLETION: u32 = 0x4;
    pub(crate) const ZERO_COPY: u32 = 0x8;
    const KNOWN: u32 = Self::VSYNC | Self::HW_CLOCK | Self::HW_COMPLETION | Self::ZERO_COPY;

    pub(crate) const fn from_bits(bits: u32) -> Self {
        Self(bits & Self::KNOWN)
    }

    pub(crate) const fn bits(self) -> u32 {
        self.0
    }

    pub const fn vsync(self) -> bool {
        self.0 & Self::VSYNC != 0
    }

    pub const fn hw_clock(self) -> bool {
        self.0 & Self::HW_CLOCK != 0
    }

    pub const fn hw_completion(self) -> bool {
        self.0 & Self::HW_COMPLETION != 0
    }

    pub const fn zero_copy(self) -> bool {
        self.0 & Self::ZERO_COPY != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Submission {
    pub(crate) submitted_at: Instant,
    pub(crate) captured_at: Option<Instant>,
    pub(crate) target: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Presented {
    pub(crate) submission: Submission,
    pub(crate) shown_at: Instant,
    pub(crate) refresh: Option<Duration>,
    pub(crate) flags: PresentationFlags,
}

impl Presented {
    pub(crate) fn display_latency(&self) -> Duration {
        self.shown_at
            .saturating_duration_since(self.submission.submitted_at)
    }

    pub(crate) fn capture_to_display(&self) -> Option<Duration> {
        self.submission
            .captured_at
            .map(|captured| self.shown_at.saturating_duration_since(captured))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feedback {
    Presented(Presented),
    Discarded(Submission),
}

pub(crate) fn instant_of(stamp: Duration, clock_now: Duration, now: Instant) -> Instant {
    match clock_now.checked_sub(stamp) {
        Some(ago) => now.checked_sub(ago).unwrap_or(now),
        None => now + stamp.saturating_sub(clock_now),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Phase {
    vblank: Instant,
    period: Option<Duration>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct VblankModel {
    phase: Option<Phase>,
}

impl VblankModel {
    pub(crate) fn observe(&mut self, presented: &Presented) {
        match presented.refresh {
            Some(period) if presented.flags.vsync() => {
                self.phase = Some(Phase {
                    vblank: presented.shown_at,
                    period: Some(period),
                });
            }
            Some(_) => {}
            None => {
                self.phase = Some(Phase {
                    vblank: presented.shown_at,
                    period: None,
                });
            }
        }
    }

    pub(crate) fn period(&self, now: Instant) -> Option<Duration> {
        self.fresh(now)?.period
    }

    pub(crate) fn next_at_or_after(&self, at: Instant, now: Instant) -> Option<Instant> {
        let phase = self.fresh(now)?;
        let period = phase.period?.as_nanos();

        if at >= phase.vblank {
            let elapsed = at.duration_since(phase.vblank).as_nanos();
            let periods = elapsed.div_ceil(period);
            Some(phase.vblank + nanos(periods * period))
        } else {
            let ahead = phase.vblank.duration_since(at).as_nanos();
            let periods = ahead / period;
            phase.vblank.checked_sub(nanos(periods * period))
        }
    }

    fn fresh(&self, now: Instant) -> Option<Phase> {
        self.phase
            .filter(|phase| now.saturating_duration_since(phase.vblank) <= STALE_AFTER)
    }
}

fn nanos(value: u128) -> Duration {
    Duration::from_nanos(u64::try_from(value).unwrap_or(u64::MAX))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Slot {
    pub(crate) due: Instant,
    pub(crate) target: Option<Instant>,
}

impl Slot {
    fn now(at: Instant) -> Self {
        Self {
            due: at,
            target: None,
        }
    }
}

#[derive(Debug, Default)]
struct DelayWindow {
    delays: VecDeque<Duration>,
}

impl DelayWindow {
    fn record(&mut self, delay: Duration) -> Duration {
        if self.delays.len() == DELAY_WINDOW {
            self.delays.pop_front();
        }
        self.delays.push_back(delay);

        self.delays.iter().copied().min().unwrap_or(delay)
    }
}

#[derive(Debug)]
pub(crate) struct Pacer {
    options: PacingOptions,
    vblank: VblankModel,
    delays: DelayWindow,
    margin: Duration,
}

impl Pacer {
    pub(crate) fn new(options: PacingOptions) -> Self {
        Self {
            options,
            vblank: VblankModel::default(),
            delays: DelayWindow::default(),
            margin: options.margin,
        }
    }

    pub(crate) fn margin(&self) -> Duration {
        self.margin
    }

    pub(crate) fn observe(&mut self, presented: &Presented) {
        self.vblank.observe(presented);

        let (Some(target), Some(period)) = (presented.submission.target, presented.refresh) else {
            return;
        };
        let missed = presented.shown_at > target + period / 2;
        if missed {
            self.margin = (self.margin + period / MARGIN_STEP_DIVISOR).min(period);
        } else if self.margin > self.options.margin {
            let excess = self.margin.saturating_sub(self.options.margin);
            self.margin -= excess / MARGIN_RELAX_DIVISOR + Duration::from_nanos(1).min(excess);
        }
    }

    pub(crate) fn schedule(&mut self, arrival: Instant, captured_at: Option<Instant>) -> Slot {
        match self.options.mode {
            PresentPacing::Off => Slot::now(arrival),
            PresentPacing::Latency => self.before_next_vblank(arrival, arrival),
            PresentPacing::Smooth => self.at_host_cadence(arrival, captured_at),
        }
    }

    fn before_next_vblank(&self, earliest: Instant, now: Instant) -> Slot {
        let Some(vblank) = self.vblank.next_at_or_after(earliest + self.margin, now) else {
            return Slot::now(earliest);
        };

        Slot {
            due: vblank.checked_sub(self.margin).unwrap_or(earliest).max(now),
            target: Some(vblank),
        }
    }

    fn at_host_cadence(&mut self, arrival: Instant, captured_at: Option<Instant>) -> Slot {
        let Some(captured) = captured_at else {
            return Slot::now(arrival);
        };

        let delay = arrival.saturating_duration_since(captured);
        let baseline = self.delays.record(delay);
        let ideal = (captured + baseline + self.options.jitter).max(arrival);

        match self.vblank.period(arrival) {
            None => Slot::now(ideal),
            Some(period) => {
                let earliest = ideal
                    .checked_sub(period / 2)
                    .unwrap_or(arrival)
                    .max(arrival);
                self.before_next_vblank(earliest, arrival)
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct PacedQueue<T> {
    entries: VecDeque<(Slot, T)>,
    capacity: usize,
}

impl<T> PacedQueue<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    pub(crate) fn push(&mut self, slot: Slot, item: T) -> u64 {
        let before = self.entries.len();
        self.entries.retain(|(queued, _)| queued.due < slot.due);
        let mut dropped = before - self.entries.len();

        self.entries.push_back((slot, item));
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
            dropped += 1;
        }

        u64::try_from(dropped).unwrap_or(u64::MAX)
    }

    pub(crate) fn take_due(&mut self, now: Instant) -> (Option<(Slot, T)>, u64) {
        let mut taken = None;
        let mut dropped = 0;

        while self
            .entries
            .front()
            .is_some_and(|(slot, _)| slot.due <= now)
        {
            if taken.is_some() {
                dropped += 1;
            }
            taken = self.entries.pop_front();
        }

        (taken, dropped)
    }

    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.entries.front().map(|(slot, _)| slot.due)
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFRESH: Duration = Duration::from_micros(16_667);
    const MARGIN: Duration = Duration::from_millis(3);
    const JITTER: Duration = Duration::from_millis(8);

    fn options(mode: PresentPacing) -> PacingOptions {
        PacingOptions {
            mode,
            margin: MARGIN,
            jitter: JITTER,
        }
    }

    fn earlier(at: Instant, by: Duration) -> Instant {
        at.checked_sub(by).unwrap()
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn presented(at: Instant, refresh: Option<Duration>, vsync: bool) -> Presented {
        Presented {
            submission: Submission {
                submitted_at: at,
                captured_at: None,
                target: None,
            },
            shown_at: at,
            refresh,
            flags: PresentationFlags::from_bits(if vsync { PresentationFlags::VSYNC } else { 0 }),
        }
    }

    fn fixed(base: Instant) -> Pacer {
        let mut pacer = Pacer::new(options(PresentPacing::Latency));
        pacer.observe(&presented(base, Some(REFRESH), true));
        pacer
    }

    #[test]
    fn a_timestamp_maps_onto_the_instant_clock_either_side_of_now() {
        let now = Instant::now() + ms(1_000);

        let past = instant_of(ms(90), ms(100), now);
        let future = instant_of(ms(105), ms(100), now);
        let same = instant_of(ms(100), ms(100), now);

        assert_eq!(past, earlier(now, ms(10)));
        assert_eq!(future, now + ms(5));
        assert_eq!(same, now);
    }

    #[test]
    fn the_next_vblank_is_found_on_either_side_of_the_observed_one() {
        let base = Instant::now() + ms(100);
        let mut model = VblankModel::default();
        model.observe(&presented(base, Some(REFRESH), true));

        let after = model.next_at_or_after(base + ms(20), base);
        let exact = model.next_at_or_after(base + REFRESH, base);
        let before = model.next_at_or_after(earlier(base, ms(20)), base);

        assert_eq!(after, Some(base + REFRESH * 2));
        assert_eq!(exact, Some(base + REFRESH));
        assert_eq!(before, Some(earlier(base, REFRESH)));
    }

    #[test]
    fn an_unsynchronised_presentation_keeps_the_previous_phase() {
        let base = Instant::now();
        let mut model = VblankModel::default();
        model.observe(&presented(base, Some(REFRESH), true));

        model.observe(&presented(base + ms(5), Some(REFRESH), false));

        assert_eq!(
            model.next_at_or_after(base + ms(1), base),
            Some(base + REFRESH)
        );
    }

    #[test]
    fn a_zero_refresh_means_variable_and_a_stale_phase_means_unknown() {
        let base = Instant::now();
        let mut model = VblankModel::default();
        model.observe(&presented(base, None, true));

        let variable = model.period(base);

        model.observe(&presented(base, Some(REFRESH), true));
        let stale = model.period(base + STALE_AFTER + ms(1));

        assert_eq!(variable, None);
        assert_eq!(
            model.next_at_or_after(base + ms(1), base),
            Some(base + REFRESH)
        );
        assert_eq!(stale, None);
    }

    #[test]
    fn off_submits_on_arrival_whatever_the_model() {
        let base = Instant::now();
        let mut pacer = Pacer::new(options(PresentPacing::Off));
        pacer.observe(&presented(base, Some(REFRESH), true));

        let slot = pacer.schedule(base + ms(2), Some(base));

        assert_eq!(slot, Slot::now(base + ms(2)));
    }

    #[test]
    fn latency_submits_one_margin_before_the_first_reachable_vblank() {
        let base = Instant::now();
        let mut pacer = fixed(base);

        let early = pacer.schedule(base + ms(2), None);
        let late = pacer.schedule(base + ms(15), None);

        assert_eq!(early.target, Some(base + REFRESH));
        assert_eq!(early.due, earlier(base + REFRESH, MARGIN));
        assert_eq!(late.target, Some(base + REFRESH * 2));
        assert_eq!(late.due, earlier(base + REFRESH * 2, MARGIN));
    }

    #[test]
    fn latency_without_a_known_refresh_submits_on_arrival() {
        let base = Instant::now();
        let mut pacer = Pacer::new(options(PresentPacing::Latency));

        let unknown = pacer.schedule(base, None);
        pacer.observe(&presented(base, None, true));
        let variable = pacer.schedule(base + ms(1), None);

        assert_eq!(unknown, Slot::now(base));
        assert_eq!(variable, Slot::now(base + ms(1)));
    }

    #[test]
    fn a_missed_vblank_widens_the_margin_and_hits_relax_it_back() {
        let base = Instant::now();
        let mut pacer = fixed(base);
        let target = base + REFRESH;
        let mut late = presented(target + REFRESH, Some(REFRESH), true);
        late.submission.target = Some(target);

        pacer.observe(&late);
        let widened = pacer.margin();

        for step in 2..400_u32 {
            let vblank = base + REFRESH * step;
            let mut hit = presented(vblank, Some(REFRESH), true);
            hit.submission.target = Some(vblank);
            pacer.observe(&hit);
        }

        assert_eq!(widened, MARGIN + REFRESH / MARGIN_STEP_DIVISOR);
        assert_eq!(pacer.margin(), MARGIN);
    }

    #[test]
    fn the_margin_never_grows_past_one_refresh() {
        let base = Instant::now();
        let mut pacer = fixed(base);

        for step in 1..20_u32 {
            let mut late = presented(base + REFRESH * (step + 1), Some(REFRESH), true);
            late.submission.target = Some(base + REFRESH * step);
            pacer.observe(&late);
        }

        assert_eq!(pacer.margin(), REFRESH);
    }

    #[test]
    fn smooth_on_a_variable_display_holds_a_constant_capture_to_submit_delay() {
        let base = Instant::now();
        let mut pacer = Pacer::new(options(PresentPacing::Smooth));
        pacer.observe(&presented(base, None, true));
        let interval = ms(8);
        let network = [ms(10), ms(14), ms(10), ms(17), ms(11)];

        let delays = network
            .iter()
            .enumerate()
            .map(|(index, delay)| {
                let captured = base + interval * u32::try_from(index).unwrap();
                let slot = pacer.schedule(captured + *delay, Some(captured));
                (slot.due - captured, slot.target)
            })
            .collect::<Vec<_>>();

        assert!(
            delays
                .iter()
                .all(|(delay, target)| *delay == ms(10) + JITTER && target.is_none())
        );
    }

    #[test]
    fn smooth_presents_late_frames_at_once() {
        let base = Instant::now();
        let mut pacer = Pacer::new(options(PresentPacing::Smooth));
        pacer.schedule(base + ms(10), Some(base));

        let late = pacer.schedule(base + ms(40), Some(base + ms(8)));

        assert_eq!(late, Slot::now(base + ms(40)));
    }

    #[test]
    fn smooth_on_a_fixed_display_targets_the_vblank_nearest_the_ideal_time() {
        let base = Instant::now();
        let mut pacer = Pacer::new(options(PresentPacing::Smooth));
        pacer.observe(&presented(base, Some(REFRESH), true));
        let captured = base + ms(1);

        let slot = pacer.schedule(captured + ms(5), Some(captured));

        let ideal = captured + ms(5) + JITTER;
        let target = slot.target.unwrap();
        assert_eq!(target, base + REFRESH);
        assert!(target.max(ideal) - target.min(ideal) <= REFRESH / 2);
        assert_eq!(slot.due, earlier(target, MARGIN));
    }

    #[test]
    fn smooth_on_a_fixed_display_never_targets_an_unreachable_vblank() {
        let base = Instant::now();
        let mut pacer = Pacer::new(PacingOptions {
            jitter: Duration::ZERO,
            ..options(PresentPacing::Smooth)
        });
        pacer.observe(&presented(base, Some(REFRESH), true));
        let arrival = earlier(base + REFRESH, ms(1));

        let slot = pacer.schedule(arrival, Some(earlier(arrival, ms(4))));

        assert_eq!(slot.target, Some(base + REFRESH * 2));
        assert!(slot.due >= arrival);
    }

    #[test]
    fn smooth_without_a_capture_time_submits_on_arrival() {
        let base = Instant::now();
        let mut pacer = Pacer::new(options(PresentPacing::Smooth));

        assert_eq!(pacer.schedule(base, None), Slot::now(base));
    }

    fn slot(due: Instant) -> Slot {
        Slot { due, target: None }
    }

    #[test]
    fn a_frame_replaces_queued_frames_due_no_earlier() {
        let base = Instant::now();
        let mut queue = PacedQueue::new(PACED_FRAMES);

        let first = queue.push(slot(base + ms(10)), 1);
        let replaced = queue.push(slot(base + ms(10)), 2);
        let behind = queue.push(slot(base + ms(20)), 3);

        assert_eq!((first, replaced, behind), (0, 1, 0));
        assert_eq!(queue.next_due(), Some(base + ms(10)));
        assert_eq!(
            queue.take_due(base + ms(10)).0.map(|(_, item)| item),
            Some(2)
        );
        assert_eq!(queue.next_due(), Some(base + ms(20)));
    }

    #[test]
    fn the_queue_drops_its_oldest_frame_when_full() {
        let base = Instant::now();
        let mut queue = PacedQueue::new(PACED_FRAMES);

        queue.push(slot(base + ms(10)), 1);
        queue.push(slot(base + ms(20)), 2);
        let dropped = queue.push(slot(base + ms(30)), 3);

        assert_eq!(dropped, 1);
        assert_eq!(queue.next_due(), Some(base + ms(20)));
    }

    #[test]
    fn taking_draws_the_newest_due_frame_and_counts_the_ones_it_skips() {
        let base = Instant::now();
        let mut queue = PacedQueue::new(3);
        queue.push(slot(base + ms(10)), 1);
        queue.push(slot(base + ms(20)), 2);
        queue.push(slot(base + ms(30)), 3);

        let early = queue.take_due(base + ms(5));
        let (taken, skipped) = queue.take_due(base + ms(25));

        assert!(early.0.is_none());
        assert_eq!(taken.map(|(_, item)| item), Some(2));
        assert_eq!(skipped, 1);
        assert_eq!(queue.next_due(), Some(base + ms(30)));

        queue.clear();
        assert_eq!(queue.next_due(), None);
    }
}
