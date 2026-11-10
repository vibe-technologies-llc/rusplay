use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use rusplay_priority::{ThreadRole, ThreadTuning};

#[derive(Debug, Clone, Default)]
pub(crate) struct TuningRefusals(Arc<AtomicU64>);

impl TuningRefusals {
    pub(crate) fn tune(&self, tuning: ThreadTuning, role: ThreadRole) {
        let refused = tuning.apply(role).len() as u64;

        self.0.fetch_add(refused, Ordering::Relaxed);
    }

    pub(crate) fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn of(count: u64) -> Self {
        Self(Arc::new(AtomicU64::new(count)))
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use rusplay_config::NetworkConfig;

    use super::*;
    use crate::tests::{TEST_NICE, current_thread_nice, network_with_nice, refusals_of};

    #[test]
    fn tuning_applies_to_the_calling_thread_and_a_granted_request_counts_nothing() {
        let refusals = TuningRefusals::default();
        let tally = refusals.clone();
        let tuning = ThreadTuning::from_config(&network_with_nice(TEST_NICE));

        let nice = thread::spawn(move || {
            tally.tune(tuning, ThreadRole::Frame);
            current_thread_nice()
        })
        .join()
        .unwrap();

        assert_eq!(nice, i32::from(TEST_NICE));
        assert_eq!(refusals.count(), 0);
    }

    #[test]
    fn every_refused_request_is_counted() {
        let network = NetworkConfig {
            frame_thread_nice: -20,
            ..NetworkConfig::default()
        };
        let refusals = TuningRefusals::default();
        let tally = refusals.clone();
        let tuning = ThreadTuning::from_config(&network);

        thread::spawn(move || {
            tally.tune(tuning, ThreadRole::TimerDriven);
            tally.tune(tuning, ThreadRole::TimerDriven);
        })
        .join()
        .unwrap();

        assert_eq!(
            refusals.count(),
            2 * refusals_of(&network, ThreadRole::TimerDriven)
        );
    }
}
