mod error;
mod rtkit;

use std::num::NonZeroU64;

pub use error::PriorityError;
use rtkit::{RealtimeService, Rtkit, request_realtime};
use rusplay_config::NetworkConfig;
use rustix::{io::Errno, process, thread::set_current_timer_slack};

const NANOS_PER_MICRO: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadRole {
    Frame,
    TimerDriven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadTuning {
    nice: i8,
    rtkit_priority: Option<u8>,
    timer_slack: Option<NonZeroU64>,
}

impl ThreadTuning {
    pub fn from_config(network: &NetworkConfig) -> Self {
        let slack_nanos = u64::from(network.send_timer_slack_us) * NANOS_PER_MICRO;

        Self {
            nice: network.frame_thread_nice,
            rtkit_priority: Some(network.rtkit_priority).filter(|priority| *priority != 0),
            timer_slack: NonZeroU64::new(slack_nanos),
        }
    }

    pub fn apply(self, role: ThreadRole) -> Vec<PriorityError> {
        self.apply_with(role, set_current_nice, Rtkit::connect)
    }

    fn apply_with<S: RealtimeService>(
        self,
        role: ThreadRole,
        set_nice: impl FnOnce(i8) -> Result<(), Errno>,
        connect: impl FnOnce() -> Result<S, PriorityError>,
    ) -> Vec<PriorityError> {
        let mut failures = Vec::new();

        if self.nice != 0
            && let Err(source) = set_nice(self.nice)
        {
            failures.push(PriorityError::Nice {
                nice: self.nice,
                source,
            });
            failures.extend(self.fall_back_to_rtkit(connect));
        }

        if role == ThreadRole::TimerDriven
            && let Some(slack) = self.timer_slack
            && let Err(source) = set_current_timer_slack(Some(slack))
        {
            failures.push(PriorityError::TimerSlack {
                nanos: slack.get(),
                source,
            });
        }

        failures
    }

    fn fall_back_to_rtkit<S: RealtimeService>(
        self,
        connect: impl FnOnce() -> Result<S, PriorityError>,
    ) -> Option<PriorityError> {
        let priority = self.rtkit_priority?;

        connect()
            .and_then(|service| request_realtime(&service, priority))
            .err()
    }
}

fn set_current_nice(nice: i8) -> Result<(), Errno> {
    process::setpriority_process(None, i32::from(nice))
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, thread};

    use rustix::thread::current_timer_slack;

    use super::*;
    use crate::rtkit::RealtimeLimits;

    impl<S: RealtimeService> RealtimeService for &S {
        fn limits(&self) -> Result<RealtimeLimits, PriorityError> {
            (**self).limits()
        }

        fn make_realtime(&self, thread: u64, priority: u32) -> Result<(), PriorityError> {
            (**self).make_realtime(thread, priority)
        }
    }

    struct Granting(Cell<Option<u32>>);

    impl RealtimeService for Granting {
        fn limits(&self) -> Result<RealtimeLimits, PriorityError> {
            Ok(RealtimeLimits {
                max_priority: 20,
                time_limit_us: u64::MAX,
            })
        }

        fn make_realtime(&self, _thread: u64, priority: u32) -> Result<(), PriorityError> {
            self.0.set(Some(priority));
            Ok(())
        }
    }

    fn tuning(nice: i8, slack_us: u32, rtkit_priority: u8) -> ThreadTuning {
        ThreadTuning::from_config(&NetworkConfig {
            frame_thread_nice: nice,
            send_timer_slack_us: slack_us,
            rtkit_priority,
            ..NetworkConfig::default()
        })
    }

    fn on_a_fresh_thread<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
        thread::spawn(body).join().unwrap()
    }

    fn unreachable_bus() -> Result<Granting, PriorityError> {
        Err(PriorityError::RtkitNoRealtime)
    }

    #[test]
    fn a_frame_thread_takes_the_configured_nice_value() {
        let (nice, failures) = on_a_fresh_thread(|| {
            let failures = tuning(7, 0, 0).apply(ThreadRole::Frame);
            (process::getpriority_process(None).unwrap(), failures.len())
        });

        assert_eq!(nice, 7);
        assert_eq!(failures, 0);
    }

    #[test]
    fn nice_zero_leaves_the_thread_alone() {
        let (unchanged, failures) = on_a_fresh_thread(|| {
            let before = process::getpriority_process(None).unwrap();
            let failures = tuning(0, 0, 10).apply(ThreadRole::Frame);
            (
                process::getpriority_process(None).unwrap() == before,
                failures.len(),
            )
        });

        assert!(unchanged);
        assert_eq!(failures, 0);
    }

    #[test]
    fn only_a_timer_driven_thread_lowers_its_timer_slack() {
        let (timer_driven, frame) = on_a_fresh_thread(|| {
            tuning(0, 1, 0).apply(ThreadRole::Frame);
            let frame = current_timer_slack().unwrap();
            tuning(0, 1, 0).apply(ThreadRole::TimerDriven);
            (current_timer_slack().unwrap(), frame)
        });

        assert_eq!(timer_driven, 1_000);
        assert_ne!(frame, 1_000);
    }

    #[test]
    fn a_refused_nice_is_reported_and_the_thread_keeps_running() {
        let failures = tuning(-20, 0, 0).apply_with(
            ThreadRole::Frame,
            |_| Err(Errno::ACCESS),
            unreachable_bus,
        );

        assert!(matches!(
            failures.as_slice(),
            [PriorityError::Nice {
                nice: -20,
                source: Errno::ACCESS
            }]
        ));
    }

    #[test]
    fn a_refused_nice_falls_back_to_the_configured_realtime_priority() {
        let service = Granting(Cell::new(None));

        let failures = tuning(-5, 0, 30).apply_with(
            ThreadRole::Frame,
            |_| Err(Errno::ACCESS),
            || Ok(&service),
        );

        assert!(matches!(failures.as_slice(), [PriorityError::Nice { .. }]));
        assert_eq!(service.0.get(), Some(20));
    }

    #[test]
    fn a_granted_nice_never_asks_rtkit() {
        let failures = tuning(-5, 0, 30).apply_with(
            ThreadRole::Frame,
            |_| Ok(()),
            || -> Result<Granting, PriorityError> { panic!("rtkit must not be contacted") },
        );

        assert!(failures.is_empty());
    }

    #[test]
    fn an_unreachable_rtkit_adds_its_own_failure() {
        let failures = tuning(-5, 0, 30).apply_with(
            ThreadRole::Frame,
            |_| Err(Errno::ACCESS),
            unreachable_bus,
        );

        assert!(matches!(
            failures.as_slice(),
            [PriorityError::Nice { .. }, PriorityError::RtkitNoRealtime]
        ));
    }

    #[test]
    fn rtkit_is_off_unless_a_priority_is_configured() {
        let failures = tuning(-5, 0, 0).apply_with(
            ThreadRole::Frame,
            |_| Err(Errno::ACCESS),
            || -> Result<Granting, PriorityError> { panic!("rtkit must not be contacted") },
        );

        assert_eq!(failures.len(), 1);
    }
}
