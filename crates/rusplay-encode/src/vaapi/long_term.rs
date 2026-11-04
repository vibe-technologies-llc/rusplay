use crate::OutputIndex;

const LONG_TERM_INTERVAL: u64 = 8;
const LONG_TERM_STALE: u64 = 60;
const MAX_RECOVERY_DISTANCE: u64 = 120;
pub(super) const RECONSTRUCTED_SURFACES: usize = 3;
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
    LongTerm(Held),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FramePlan {
    pub(super) reference: Reference,
    pub(super) kept_long_term: Option<Held>,
    pub(super) mark_long_term: bool,
}

#[derive(Debug)]
pub(super) struct LongTermReferences {
    enabled: bool,
    previous: Option<Held>,
    long_term: Option<Held>,
    next_output: OutputIndex,
    acknowledged: Option<OutputIndex>,
    plan: FramePlan,
}

impl LongTermReferences {
    pub(super) const fn new(enabled: bool) -> Self {
        Self {
            enabled,
            previous: None,
            long_term: None,
            next_output: OutputIndex(0),
            acknowledged: None,
            plan: FramePlan {
                reference: Reference::Intra,
                kept_long_term: None,
                mark_long_term: enabled,
            },
        }
    }

    pub(super) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn holds(&self, surface: u32) -> bool {
        [self.previous, self.long_term]
            .iter()
            .flatten()
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

    pub(super) fn recover(&mut self, last_good: OutputIndex) -> Option<OutputIndex> {
        if !self.enabled {
            return None;
        }

        self.acknowledge(last_good);

        if let Some(previous) = self.previous.filter(|held| held.output <= last_good) {
            return Some(previous.output);
        }

        let distance = |held: &Held| self.next_output.0.saturating_sub(held.output.0);
        let reference = self
            .long_term
            .filter(|held| held.output <= last_good && distance(held) <= MAX_RECOVERY_DISTANCE)?;
        self.previous = None;

        Some(reference.output)
    }

    fn long_term_due(&self) -> bool {
        let Some(held) = self.long_term else {
            return true;
        };
        let age = self.next_output.0.saturating_sub(held.output.0);
        let confirmed = self.acknowledged.is_some_and(|known| held.output <= known);

        age >= LONG_TERM_INTERVAL
            && (self.acknowledged.is_none() || confirmed || age >= LONG_TERM_STALE)
    }

    pub(super) fn begin(&mut self, keyframe: bool) {
        if keyframe {
            self.previous = None;
            self.long_term = None;
            self.acknowledged = None;
            self.plan = FramePlan {
                reference: Reference::Intra,
                kept_long_term: None,
                mark_long_term: self.enabled,
            };
            return;
        }

        let (reference, kept_long_term) = match (self.previous, self.long_term) {
            (Some(previous), kept) => (Reference::Previous(previous), kept),
            (None, Some(long_term)) => (Reference::LongTerm(long_term), None),
            (None, None) => (Reference::Intra, None),
        };

        self.plan = FramePlan {
            reference,
            kept_long_term,
            mark_long_term: self.enabled && self.previous.is_some() && self.long_term_due(),
        };
    }

    pub(super) fn commit(&mut self, surface: u32, order: i32) {
        let committed = Held {
            surface,
            order,
            output: self.next_output,
        };

        if self.plan.mark_long_term {
            self.long_term = Some(committed);
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
    fn a_keyframe_starts_long_term_and_the_next_frame_references_it() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 1);
        references.begin(false);

        assert!(matches!(
            references.plan().reference,
            Reference::LongTerm(Held {
                output: OutputIndex(0),
                ..
            })
        ));
        assert!(!references.plan().mark_long_term);
    }

    #[test]
    fn long_term_candidates_come_every_interval_without_acknowledgements() {
        let mut references = LongTermReferences::new(true);
        let mut marked = Vec::new();

        for index in 0..20_u64 {
            references.begin(index == 0);
            if references.plan().mark_long_term {
                marked.push(index);
            }
            references.commit(1, i32::try_from(index).unwrap());
        }

        assert_eq!(marked, [0, 8, 16]);
    }

    #[test]
    fn an_unconfirmed_candidate_waits_for_its_acknowledgement() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 9);
        references.acknowledge(OutputIndex(4));
        let mut marked = Vec::new();
        for index in 9..80_u64 {
            references.begin(false);
            if references.plan().mark_long_term {
                marked.push(index);
            }
            references.commit(1, i32::try_from(index).unwrap());
        }

        assert_eq!(marked[0], 68, "{marked:?}");
    }

    #[test]
    fn a_confirmed_long_term_frame_is_replaced_at_the_interval() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 9);
        references.acknowledge(OutputIndex(8));
        references.begin(false);
        assert!(!references.plan().mark_long_term);
        references.commit(1, 9);
        run(&mut references, 6);
        references.begin(false);

        assert!(references.plan().mark_long_term);
    }

    #[test]
    fn recovery_references_the_long_term_frame_and_forgets_the_previous_one() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 12);
        let reference = references.recover(OutputIndex(9));
        references.begin(false);
        let plan = references.plan();

        assert_eq!(reference, Some(OutputIndex(8)));
        assert!(matches!(
            plan.reference,
            Reference::LongTerm(Held {
                output: OutputIndex(8),
                ..
            })
        ));
        assert_eq!(plan.kept_long_term, None);
        assert!(!plan.mark_long_term);
    }

    #[test]
    fn recovery_needs_a_reference_at_or_before_the_last_good_frame() {
        let mut references = LongTermReferences::new(true);

        run(&mut references, 12);

        assert_eq!(references.recover(OutputIndex(7)), None);
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
        references.long_term = Some(Held {
            surface: 2,
            order: 0,
            output: OutputIndex(0),
        });
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
        assert!(references.plan().mark_long_term);
    }

    #[test]
    fn disabled_references_stay_on_the_previous_frame_and_refuse_recovery() {
        let mut references = LongTermReferences::new(false);

        for index in 0..20_u64 {
            references.begin(index == 0);
            assert!(!references.plan().mark_long_term);
            assert_eq!(references.plan().kept_long_term, None);
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
