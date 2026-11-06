use crate::OutputIndex;

const LONG_TERM_INTERVAL: u64 = 8;
const LONG_TERM_STALE: u64 = 60;
const MAX_RECOVERY_DISTANCE: u64 = 120;
pub(super) const LONG_TERM_SLOTS: usize = 2;
pub(super) const RECONSTRUCTED_SURFACES: usize = LONG_TERM_SLOTS + 2;
const VERIFIED_MESA: (u32, u32) = (26, 2);
const VERIFIED_DRIVER: &str = "radeonsi";
const VERIFIED_GFX_LEVEL: &str = "gfx12";

fn mesa_version(vendor: &str) -> Option<(u32, u32)> {
    let version = vendor.split("Mesa").nth(1)?;
    let digits = version
        .split_whitespace()
        .find(|word| word.starts_with(|first: char| first.is_ascii_digit()))?;
    let mut parts = digits.split('.');

    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

pub(super) fn driver_honours_long_term(vendor: &str) -> bool {
    let renderer = vendor.split_once('(').map_or("", |(_, renderer)| renderer);
    let mut fields = renderer.split(',').map(str::trim);

    fields.next() == Some(VERIFIED_DRIVER)
        && fields
            .next()
            .is_some_and(|gfx| gfx.starts_with(VERIFIED_GFX_LEVEL))
        && mesa_version(vendor).is_some_and(|version| version >= VERIFIED_MESA)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Held {
    pub(super) surface: u32,
    pub(super) order: i32,
    pub(super) output: OutputIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reference {
    Intra,
    Previous(Held),
    LongTerm { slot: usize, held: Held },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FramePlan {
    pub(super) reference: Reference,
    pub(super) long_term: [Option<Held>; LONG_TERM_SLOTS],
    pub(super) mark_long_term: Option<usize>,
}

impl FramePlan {
    const INTRA: Self = Self {
        reference: Reference::Intra,
        long_term: [None; LONG_TERM_SLOTS],
        mark_long_term: None,
    };

    pub(super) fn referenced_slot(&self) -> Option<usize> {
        match self.reference {
            Reference::LongTerm { slot, .. } => Some(slot),
            Reference::Intra | Reference::Previous(_) => None,
        }
    }

    pub(super) fn kept_long_term(&self) -> impl Iterator<Item = (usize, Held)> + '_ {
        let referenced = self.referenced_slot();

        self.long_term
            .iter()
            .enumerate()
            .filter(move |(slot, _)| Some(*slot) != referenced)
            .filter_map(|(slot, held)| held.map(|held| (slot, held)))
    }
}

#[derive(Debug)]
pub(super) struct LongTermReferences {
    enabled: bool,
    previous: Option<Held>,
    long_term: [Option<Held>; LONG_TERM_SLOTS],
    first_written: [Option<OutputIndex>; LONG_TERM_SLOTS],
    next_output: OutputIndex,
    acknowledged: Option<OutputIndex>,
    plan: FramePlan,
}

impl LongTermReferences {
    pub(super) const fn new(enabled: bool) -> Self {
        Self {
            enabled,
            previous: None,
            long_term: [None; LONG_TERM_SLOTS],
            first_written: [None; LONG_TERM_SLOTS],
            next_output: OutputIndex(0),
            acknowledged: None,
            plan: FramePlan {
                mark_long_term: if enabled { Some(0) } else { None },
                ..FramePlan::INTRA
            },
        }
    }

    pub(super) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn holds(&self, surface: u32) -> bool {
        self.previous
            .iter()
            .chain(self.long_term.iter().flatten())
            .any(|held| held.surface == surface)
    }

    pub(super) const fn plan(&self) -> FramePlan {
        self.plan
    }

    pub(super) fn acknowledge(&mut self, output: OutputIndex) {
        if output < self.next_output && self.acknowledged.is_none_or(|known| output > known) {
            self.acknowledged = Some(output);
        }
    }

    fn age(&self, held: Held) -> u64 {
        self.next_output.0.saturating_sub(held.output.0)
    }

    fn confirmed(&self, held: Held) -> bool {
        self.acknowledged.is_some_and(|known| held.output <= known)
    }

    pub(super) fn recover(&mut self, last_good: OutputIndex) -> Option<OutputIndex> {
        if !self.enabled {
            return None;
        }

        self.acknowledge(last_good);

        if let Some(previous) = self.previous.filter(|held| held.output <= last_good) {
            return Some(previous.output);
        }

        if self
            .first_written
            .iter()
            .flatten()
            .any(|first| *first > last_good)
        {
            return None;
        }

        let reference = self
            .long_term
            .iter()
            .flatten()
            .filter(|held| held.output <= last_good && self.age(**held) <= MAX_RECOVERY_DISTANCE)
            .map(|held| held.output)
            .max()?;
        self.previous = None;
        for slot in 0..LONG_TERM_SLOTS {
            let keep = self.long_term[slot].is_some_and(|held| held.output == reference);
            if !keep {
                self.long_term[slot] = None;
                self.first_written[slot] = None;
            }
        }

        Some(reference)
    }

    fn marking_slot(&self) -> Option<usize> {
        let keeper = (0..LONG_TERM_SLOTS)
            .filter(|&slot| self.long_term[slot].is_some_and(|held| self.confirmed(held)))
            .max_by_key(|&slot| self.long_term[slot].map(|held| held.output));
        let candidate = (0..LONG_TERM_SLOTS)
            .filter(|&slot| Some(slot) != keeper)
            .min_by_key(|&slot| self.long_term[slot].map(|held| held.output))?;
        let newest_age = self
            .long_term
            .iter()
            .flatten()
            .map(|held| self.age(*held))
            .min();
        let due = newest_age.is_none_or(|age| age >= LONG_TERM_INTERVAL);

        match self.long_term[candidate] {
            None => Some(candidate),
            Some(held) => {
                let replaceable = self.acknowledged.is_none() || self.confirmed(held);
                (due && (replaceable || self.age(held) >= LONG_TERM_STALE)).then_some(candidate)
            }
        }
    }

    fn has_empty_slot(&self) -> bool {
        self.long_term.iter().any(Option::is_none)
    }

    fn newest_long_term(&self) -> Option<(usize, Held)> {
        self.long_term
            .iter()
            .enumerate()
            .filter_map(|(slot, held)| held.map(|held| (slot, held)))
            .max_by_key(|(_, held)| held.output)
    }

    pub(super) fn begin(&mut self, keyframe: bool) {
        if keyframe {
            self.previous = None;
            self.long_term = [None; LONG_TERM_SLOTS];
            self.first_written = [None; LONG_TERM_SLOTS];
            self.acknowledged = None;
            self.plan = FramePlan {
                mark_long_term: self.enabled.then_some(0),
                ..FramePlan::INTRA
            };
            return;
        }

        let reference = match (self.previous, self.newest_long_term()) {
            (Some(previous), _) => Reference::Previous(previous),
            (None, Some((slot, held))) => Reference::LongTerm { slot, held },
            (None, None) => Reference::Intra,
        };

        self.plan = FramePlan {
            reference,
            long_term: self.long_term,
            mark_long_term: if self.enabled && (self.previous.is_some() || self.has_empty_slot()) {
                self.marking_slot()
            } else {
                None
            },
        };
    }

    pub(super) fn commit(&mut self, surface: u32, order: i32) {
        let committed = Held {
            surface,
            order,
            output: self.next_output,
        };

        if let Some(slot) = self.plan.mark_long_term {
            self.long_term[slot] = Some(committed);
            self.first_written[slot].get_or_insert(committed.output);
            self.previous = None;
        } else {
            self.previous = Some(committed);
        }
        self.next_output = self.next_output.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(references: &mut LongTermReferences, frames: u64) {
        for _ in 0..frames {
            let keyframe = references.next_output == OutputIndex(0);
            let surface = u32::try_from(references.next_output.0 % 3).unwrap();

            references.begin(keyframe);
            references.commit(surface, i32::try_from(references.next_output.0).unwrap());
        }
    }

    #[test]
    fn only_the_verified_driver_honours_long_term_marking() {
        let verified = "Mesa Gallium driver 26.2.6-arch3.1 for AMD Radeon RX 9070 XT \
                        (radeonsi, gfx1201, ACO, DRM 3.64, 7.3.1-1-cachyos)";
        let older_mesa = "Mesa Gallium driver 26.1.0 for AMD Radeon RX 9070 XT \
                          (radeonsi, gfx1201, ACO, DRM 3.64, 7.3.1)";
        let older_vcn = "Mesa Gallium driver 26.2.6 for AMD Radeon RX 6800 \
                         (radeonsi, gfx1030, ACO, DRM 3.64, 7.3.1)";
        let intel = "Intel iHD driver for Intel(R) Gen Graphics - 25.3.4";

        assert!(driver_honours_long_term(verified));
        assert!(!driver_honours_long_term(older_mesa));
        assert!(!driver_honours_long_term(older_vcn));
        assert!(!driver_honours_long_term(intel));
        assert!(!driver_honours_long_term(""));
    }

    #[test]
    fn a_keyframe_starts_long_term_and_the_next_frame_references_it_and_fills_the_second_slot() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 1);
        references.begin(false);

        assert!(matches!(
            references.plan().reference,
            Reference::LongTerm {
                slot: 0,
                held: Held {
                    output: OutputIndex(0),
                    ..
                }
            }
        ));
        assert_eq!(references.plan().mark_long_term, Some(1));
    }

    #[test]
    fn long_term_candidates_come_every_interval_without_acknowledgements() {
        let mut references = LongTermReferences::new(true);
        let mut marked = Vec::new();

        for index in 0..20_u64 {
            references.begin(index == 0);
            if references.plan().mark_long_term.is_some() {
                marked.push(index);
            }
            references.commit(1, i32::try_from(index).unwrap());
        }

        assert_eq!(marked, [0, 1, 9, 17]);
    }

    #[test]
    fn an_unconfirmed_candidate_waits_for_its_acknowledgement() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 10);
        references.acknowledge(OutputIndex(0));
        let mut marked = Vec::new();
        for index in 10..80_u64 {
            references.begin(false);
            if references.plan().mark_long_term.is_some() {
                marked.push(index);
            }
            references.commit(1, i32::try_from(index).unwrap());
        }

        assert_eq!(marked[0], 61, "{marked:?}");
    }

    #[test]
    fn a_confirmed_long_term_frame_is_replaced_at_the_interval() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 10);
        references.acknowledge(OutputIndex(9));
        references.begin(false);
        assert_eq!(references.plan().mark_long_term, None);
        references.commit(1, 10);
        run(&mut references, 6);
        references.begin(false);

        assert_eq!(references.plan().mark_long_term, Some(1));
    }

    #[test]
    fn recovery_references_the_long_term_frame_and_forgets_the_previous_one() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 12);
        let reference = references.recover(OutputIndex(9));
        references.begin(false);
        let plan = references.plan();

        assert_eq!(reference, Some(OutputIndex(9)));
        assert!(matches!(
            plan.reference,
            Reference::LongTerm {
                slot: 0,
                held: Held {
                    output: OutputIndex(9),
                    ..
                }
            }
        ));
        assert_eq!(plan.kept_long_term().count(), 0);
        assert!(plan.long_term[1].is_none());
        assert_eq!(plan.mark_long_term, Some(1));
    }

    #[test]
    fn recovery_falls_back_to_the_older_long_term_frame_and_refills_the_forgotten_slot() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 12);
        references.acknowledge(OutputIndex(5));
        let reference = references.recover(OutputIndex(7));
        references.begin(false);
        let plan = references.plan();

        assert_eq!(reference, Some(OutputIndex(1)));
        assert!(matches!(
            plan.reference,
            Reference::LongTerm {
                slot: 1,
                held: Held {
                    output: OutputIndex(1),
                    ..
                }
            }
        ));
        assert_eq!(plan.kept_long_term().count(), 0);
        assert_eq!(plan.long_term[0], None);
        assert_eq!(plan.mark_long_term, Some(0));
    }

    #[test]
    fn recovery_refuses_when_the_first_frame_of_a_slot_may_be_lost() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 4);

        assert_eq!(references.recover(OutputIndex(0)), None);
    }

    #[test]
    fn the_marked_slots_alternate_while_candidates_are_acknowledged() {
        let mut references = LongTermReferences::new(true);
        let mut slots = Vec::new();

        for index in 0..32_u64 {
            references.begin(index == 0);
            slots.extend(references.plan().mark_long_term);
            references.commit(1, i32::try_from(index).unwrap());
            references.acknowledge(OutputIndex(index));
        }

        assert_eq!(slots, [0, 1, 0, 1, 0]);
    }

    #[test]
    fn the_newest_acknowledged_slot_is_kept_and_the_other_replaced() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 10);
        references.acknowledge(OutputIndex(9));
        run(&mut references, 7);
        references.begin(false);

        assert_eq!(references.plan().mark_long_term, Some(1));
    }

    #[test]
    fn an_unacknowledged_newest_slot_is_never_the_one_replaced() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 10);
        references.acknowledge(OutputIndex(3));
        run(&mut references, 7);
        references.begin(false);

        assert_eq!(references.plan().mark_long_term, None);
        assert_eq!(
            references
                .long_term
                .map(|held| held.map(|held| held.output)),
            [Some(OutputIndex(9)), Some(OutputIndex(1))]
        );
    }

    #[test]
    fn recovery_on_the_newest_frame_changes_nothing() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 12);
        let reference = references.recover(OutputIndex(11));
        references.begin(false);

        assert_eq!(reference, Some(OutputIndex(11)));
        assert!(matches!(
            references.plan().reference,
            Reference::Previous(_)
        ));
    }

    #[test]
    fn recovery_refuses_a_reference_beyond_the_distance_limit() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 9);
        references.acknowledge(OutputIndex(4));
        for index in 9..60_u64 {
            references.begin(false);
            references.commit(1, i32::try_from(index).unwrap());
        }
        references.long_term = [
            Some(Held {
                surface: 2,
                order: 0,
                output: OutputIndex(0),
            }),
            None,
        ];
        references.next_output = OutputIndex(121);

        assert_eq!(references.recover(OutputIndex(50)), None);
    }

    #[test]
    fn a_keyframe_clears_everything_held() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 12);
        references.acknowledge(OutputIndex(11));
        references.begin(true);

        assert!(!references.holds(0));
        assert!(!references.holds(1));
        assert!(!references.holds(2));
        assert_eq!(references.acknowledged, None);
        assert_eq!(references.plan().reference, Reference::Intra);
        assert!(references.plan().mark_long_term.is_some());
    }

    #[test]
    fn disabled_references_stay_on_the_previous_frame_and_refuse_recovery() {
        let mut references = LongTermReferences::new(false);

        for index in 0..20_u64 {
            references.begin(index == 0);
            assert_eq!(references.plan().mark_long_term, None);
            assert_eq!(references.plan().kept_long_term().count(), 0);
            assert_eq!(index == 0, references.plan().reference == Reference::Intra);
            if index > 0 {
                assert!(matches!(
                    references.plan().reference,
                    Reference::Previous(_)
                ));
            }
            references.commit(1, i32::try_from(index).unwrap());
        }

        assert_eq!(references.recover(OutputIndex(19)), None);
    }

    #[test]
    fn a_future_acknowledgement_is_ignored() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 3);
        references.acknowledge(OutputIndex(3));

        assert_eq!(references.acknowledged, None);
    }
}
