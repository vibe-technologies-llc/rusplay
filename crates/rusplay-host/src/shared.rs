use std::sync::atomic::{AtomicBool, Ordering};

use rusplay_capture::Frame;

use crate::{mailbox::Mailbox, stats::Counters};

#[derive(Debug)]
pub(crate) struct Shared {
    stop: AtomicBool,
    streaming: AtomicBool,
    pub counters: Counters,
    pub mailbox: Mailbox<Frame>,
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
            counters: Counters::default(),
            mailbox: Mailbox::new(),
        }
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub(crate) fn streaming(&self) -> bool {
        self.streaming.load(Ordering::Acquire)
    }

    pub(crate) fn start(&self) {
        self.streaming.store(true, Ordering::Release);
        self.mailbox.start();
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.mailbox.close());
    }
}
