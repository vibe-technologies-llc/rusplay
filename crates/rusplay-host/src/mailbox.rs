use std::time::Duration;

use parking_lot::{Condvar, Mutex};

#[derive(Debug)]
pub(crate) struct Closed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecoveryRequest {
    pub frame: u32,
    pub last_good: u32,
}

impl RecoveryRequest {
    fn merge(self, other: Self) -> Self {
        Self {
            frame: earlier(self.frame, other.frame),
            last_good: earlier(self.last_good, other.last_good),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Delivery<T> {
    pub frame: Option<T>,
    pub keyframe_for: Option<u32>,
    pub keyframe_forced: bool,
    pub recovery_for: Option<RecoveryRequest>,
    pub acknowledged: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    pub suspensions: u32,
}

impl<T> Delivery<T> {
    pub(crate) const fn idle(suspensions: u32) -> Self {
        Self {
            frame: None,
            keyframe_for: None,
            keyframe_forced: false,
            recovery_for: None,
            acknowledged: None,
            bitrate_kbps: None,
            suspensions,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Waited<T> {
    Delivered(Delivery<T>),
    Idle,
    Closed,
}

#[derive(Debug)]
struct State<T> {
    frame: Option<T>,
    keyframe_for: Option<u32>,
    keyframe_forced: bool,
    recovery_for: Option<RecoveryRequest>,
    acknowledged: Option<u32>,
    bitrate_kbps: Option<u32>,
    suspensions: u32,
    started: bool,
    closed: bool,
}

impl<T> State<T> {
    fn ready(&self) -> bool {
        self.started
            && (self.frame.is_some()
                || self.keyframe_for.is_some()
                || self.keyframe_forced
                || self.recovery_for.is_some()
                || self.bitrate_kbps.is_some())
    }

    fn take(&mut self) -> Delivery<T> {
        Delivery {
            frame: self.frame.take(),
            keyframe_for: self.keyframe_for.take(),
            keyframe_forced: std::mem::take(&mut self.keyframe_forced),
            recovery_for: self.recovery_for.take(),
            acknowledged: self.acknowledged.take(),
            bitrate_kbps: self.bitrate_kbps.take(),
            suspensions: self.suspensions,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Mailbox<T> {
    state: Mutex<State<T>>,
    changed: Condvar,
}

impl<T> Mailbox<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                frame: None,
                keyframe_for: None,
                keyframe_forced: false,
                recovery_for: None,
                acknowledged: None,
                bitrate_kbps: None,
                suspensions: 0,
                started: false,
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn put(&self, frame: T) -> Result<Option<T>, Closed> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(Closed);
        }

        let replaced = state.frame.replace(frame);
        drop(state);
        self.changed.notify_one();

        Ok(replaced)
    }

    pub(crate) fn request_keyframe(&self, frame: u32) {
        let mut state = self.state.lock();
        state.keyframe_for = Some(
            state
                .keyframe_for
                .map_or(frame, |pending| earlier(pending, frame)),
        );
        drop(state);
        self.changed.notify_one();
    }

    pub(crate) fn request_recovery(&self, request: RecoveryRequest) {
        let mut state = self.state.lock();
        state.recovery_for = Some(
            state
                .recovery_for
                .map_or(request, |pending| pending.merge(request)),
        );
        drop(state);
        self.changed.notify_one();
    }

    pub(crate) fn acknowledge(&self, frame: u32) {
        let mut state = self.state.lock();
        state.acknowledged = Some(
            state
                .acknowledged
                .map_or(frame, |pending| later(pending, frame)),
        );
    }

    pub(crate) fn set_bitrate(&self, kbps: u32) {
        self.state.lock().bitrate_kbps = Some(kbps);
        self.changed.notify_one();
    }

    pub(crate) fn suspend(&self) {
        let mut state = self.state.lock();
        state.started = false;
        state.suspensions = state.suspensions.wrapping_add(1);
        state.keyframe_for = None;
        state.recovery_for = None;
        state.acknowledged = None;
        state.keyframe_forced = true;
    }

    pub(crate) fn suspensions(&self) -> u32 {
        self.state.lock().suspensions
    }

    pub(crate) fn start(&self) {
        self.state.lock().started = true;
        self.changed.notify_one();
    }

    pub(crate) fn close(&self) -> Option<T> {
        let mut state = self.state.lock();
        state.closed = true;
        let pending = state.frame.take();
        drop(state);
        self.changed.notify_all();

        pending
    }

    pub(crate) fn wait(&self, timeout: Duration) -> Waited<T> {
        let mut state = self.state.lock();

        if !state.ready() && !state.closed {
            self.changed.wait_while_for(
                &mut state,
                |state| !state.ready() && !state.closed,
                timeout,
            );
        }

        if state.closed {
            Waited::Closed
        } else if state.ready() {
            Waited::Delivered(state.take())
        } else {
            Waited::Idle
        }
    }
}

fn earlier(first: u32, second: u32) -> u32 {
    if second.wrapping_sub(first).cast_signed() < 0 {
        second
    } else {
        first
    }
}

fn later(first: u32, second: u32) -> u32 {
    if second.wrapping_sub(first).cast_signed() > 0 {
        second
    } else {
        first
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use super::*;

    const SHORT: Duration = Duration::from_millis(5);

    #[test]
    fn nothing_is_delivered_before_the_stream_starts() {
        let mailbox = Mailbox::new();

        mailbox.put(1).unwrap();
        let before = mailbox.wait(SHORT);
        mailbox.start();
        let after = mailbox.wait(SHORT);

        assert_eq!(before, Waited::Idle);
        assert_eq!(
            after,
            Waited::Delivered(Delivery {
                frame: Some(1),
                keyframe_for: None,
                keyframe_forced: false,
                recovery_for: None,
                acknowledged: None,
                bitrate_kbps: None,
                suspensions: 0,
            })
        );
    }

    #[test]
    fn newest_frame_replaces_the_unread_one() {
        let mailbox = Mailbox::new();
        mailbox.start();

        let first = mailbox.put(1).unwrap();
        let second = mailbox.put(2).unwrap();

        assert_eq!(first, None);
        assert_eq!(second, Some(1));
        assert!(matches!(
            mailbox.wait(SHORT),
            Waited::Delivered(Delivery { frame: Some(2), .. })
        ));
    }

    #[test]
    fn keyframe_requests_coalesce_to_the_earliest_frame() {
        let mailbox = Mailbox::<u32>::new();
        mailbox.start();

        mailbox.request_keyframe(7);
        mailbox.request_keyframe(5);
        mailbox.request_keyframe(9);

        assert_eq!(
            mailbox.wait(SHORT),
            Waited::Delivered(Delivery {
                frame: None,
                keyframe_for: Some(5),
                keyframe_forced: false,
                recovery_for: None,
                acknowledged: None,
                bitrate_kbps: None,
                suspensions: 0,
            })
        );
    }

    #[test]
    fn suspending_holds_deliveries_until_the_restart_and_then_forces_a_keyframe() {
        let mailbox = Mailbox::<u32>::new();
        mailbox.start();
        mailbox.request_keyframe(3);

        mailbox.suspend();
        mailbox.put(4).unwrap();
        let held = mailbox.wait(SHORT);
        mailbox.start();

        assert_eq!(held, Waited::Idle);
        assert_eq!(
            mailbox.wait(SHORT),
            Waited::Delivered(Delivery {
                frame: Some(4),
                keyframe_for: None,
                keyframe_forced: true,
                recovery_for: None,
                acknowledged: None,
                bitrate_kbps: None,
                suspensions: 1,
            })
        );
    }

    #[test]
    fn recovery_requests_coalesce_to_the_earliest_loss_and_reference() {
        let mailbox = Mailbox::<u32>::new();
        mailbox.start();

        mailbox.request_recovery(RecoveryRequest {
            frame: 9,
            last_good: 6,
        });
        mailbox.request_recovery(RecoveryRequest {
            frame: 8,
            last_good: 7,
        });

        assert!(matches!(
            mailbox.wait(SHORT),
            Waited::Delivered(Delivery {
                recovery_for: Some(RecoveryRequest {
                    frame: 8,
                    last_good: 6,
                }),
                ..
            })
        ));
    }

    #[test]
    fn acknowledgements_keep_the_newest_frame_and_wait_for_the_next_delivery() {
        let mailbox = Mailbox::<u32>::new();
        mailbox.start();

        mailbox.acknowledge(4);
        mailbox.acknowledge(6);
        mailbox.acknowledge(5);
        let alone = mailbox.wait(SHORT);
        mailbox.put(1).unwrap();

        assert_eq!(alone, Waited::Idle);
        assert!(matches!(
            mailbox.wait(SHORT),
            Waited::Delivered(Delivery {
                frame: Some(1),
                acknowledged: Some(6),
                ..
            })
        ));
    }

    #[test]
    fn suspending_drops_pending_recovery_and_acknowledgements() {
        let mailbox = Mailbox::<u32>::new();
        mailbox.start();
        mailbox.request_recovery(RecoveryRequest {
            frame: 3,
            last_good: 2,
        });
        mailbox.acknowledge(2);

        mailbox.suspend();
        mailbox.start();

        assert!(matches!(
            mailbox.wait(SHORT),
            Waited::Delivered(Delivery {
                recovery_for: None,
                acknowledged: None,
                keyframe_forced: true,
                ..
            })
        ));
    }

    #[test]
    fn earliest_frame_survives_wrap_around() {
        assert_eq!(earlier(u32::MAX, 2), u32::MAX);
        assert_eq!(earlier(2, u32::MAX), u32::MAX);
        assert_eq!(later(u32::MAX, 2), 2);
        assert_eq!(later(2, u32::MAX), 2);
    }

    #[test]
    fn closing_wakes_a_waiting_reader_and_refuses_writers() {
        let mailbox = Arc::new(Mailbox::<u32>::new());
        let reader = {
            let mailbox = Arc::clone(&mailbox);
            thread::spawn(move || mailbox.wait(Duration::from_secs(10)))
        };

        mailbox.close();

        assert_eq!(reader.join().unwrap(), Waited::Closed);
        assert!(mailbox.put(3).is_err());
    }
}
