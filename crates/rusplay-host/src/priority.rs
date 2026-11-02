use std::num::NonZeroU64;

use rusplay_config::NetworkConfig;
use rustix::{process, thread::set_current_timer_slack};

use crate::Stage;

const NANOS_PER_MICRO: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThreadTuning {
    nice: i8,
    send_timer_slack: Option<NonZeroU64>,
}

impl ThreadTuning {
    pub(crate) fn from_config(network: &NetworkConfig) -> Self {
        let slack_nanos = u64::from(network.send_timer_slack_us) * NANOS_PER_MICRO;

        Self {
            nice: network.frame_thread_nice,
            send_timer_slack: NonZeroU64::new(slack_nanos),
        }
    }

    pub(crate) fn apply(self, stage: Stage) {
        if !matches!(stage, Stage::Encode | Stage::Send | Stage::Control) {
            return;
        }

        if self.nice != 0 {
            process::setpriority_process(None, i32::from(self.nice)).ok();
        }

        if stage == Stage::Send && self.send_timer_slack.is_some() {
            set_current_timer_slack(self.send_timer_slack).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use rustix::thread::current_timer_slack;

    use super::*;

    fn tuning(nice: i8, slack_us: u32) -> ThreadTuning {
        ThreadTuning::from_config(&NetworkConfig {
            frame_thread_nice: nice,
            send_timer_slack_us: slack_us,
            ..NetworkConfig::default()
        })
    }

    fn on_a_fresh_thread<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
        thread::spawn(body).join().unwrap()
    }

    #[test]
    fn a_frame_stage_takes_the_configured_nice_value() {
        let nice = on_a_fresh_thread(|| {
            tuning(7, 0).apply(Stage::Encode);
            process::getpriority_process(None).unwrap()
        });

        assert_eq!(nice, 7);
    }

    #[test]
    fn nice_zero_and_other_stages_leave_the_thread_alone() {
        let (zero, audio) = on_a_fresh_thread(|| {
            let before = process::getpriority_process(None).unwrap();
            tuning(0, 0).apply(Stage::Send);
            let zero = process::getpriority_process(None).unwrap() == before;
            tuning(9, 0).apply(Stage::Audio);
            (zero, process::getpriority_process(None).unwrap() == before)
        });

        assert!(zero);
        assert!(audio);
    }

    #[test]
    fn only_the_send_stage_lowers_its_timer_slack() {
        let (send, control) = on_a_fresh_thread(|| {
            tuning(0, 1).apply(Stage::Control);
            let control = current_timer_slack().unwrap();
            tuning(0, 1).apply(Stage::Send);
            (current_timer_slack().unwrap(), control)
        });

        assert_eq!(send, 1_000);
        assert_ne!(control, 1_000);
    }

    #[test]
    fn a_refused_request_is_silent() {
        on_a_fresh_thread(|| {
            tuning(-20, 1).apply(Stage::Send);
        });
    }
}
