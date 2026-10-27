use std::time::Duration;

use parking_lot::{Condvar, Mutex};

#[derive(Debug)]
pub(crate) struct Closed;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Delivery<T> {
    pub frame: Option<T>,
    pub keyframe_for: Option<u32>,
    pub keyframe_forced: bool,
    pub bitrate_kbps: Option<u32>,
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
    bitrate_kbps: Option<u32>,
    started: bool,
    closed: bool,
}

impl<T> State<T> {
    fn ready(&self) -> bool {
        self.started
            && (self.frame.is_some()
                || self.keyframe_for.is_some()
                || self.keyframe_forced
                || self.bitrate_kbps.is_some())
    }

    fn take(&mut self) -> Delivery<T> {
        Delivery {
            frame: self.frame.take(),
            keyframe_for: self.keyframe_for.take(),
            keyframe_forced: std::mem::take(&mut self.keyframe_forced),
            bitrate_kbps: self.bitrate_kbps.take(),
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
                bitrate_kbps: None,
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

    pub(crate) fn set_bitrate(&self, kbps: u32) {
        self.state.lock().bitrate_kbps = Some(kbps);
        self.changed.notify_one();
    }

    pub(crate) fn suspend(&self) {
        let mut state = self.state.lock();
        state.started = false;
        state.keyframe_for = None;
        state.keyframe_forced = true;
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
                bitrate_kbps: None,
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
                bitrate_kbps: None,
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
                bitrate_kbps: None,
            })
        );
    }

    #[test]
    fn earliest_frame_survives_wrap_around() {
        assert_eq!(earlier(u32::MAX, 2), u32::MAX);
        assert_eq!(earlier(2, u32::MAX), u32::MAX);
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
