use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use rusplay_capture::Frame;

use crate::{link::ClientLink, mailbox::Mailbox, stats::Counters};

#[derive(Debug)]
pub(crate) struct Shared {
    stop: AtomicBool,
    streaming: AtomicBool,
    pub client: Arc<ClientLink>,
    pub counters: Counters,
    pub mailbox: Mailbox<Frame>,
}

impl Shared {
    pub(crate) fn new(client: SocketAddr) -> Self {
        Self {
            stop: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
            client: Arc::new(ClientLink::new(client)),
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

    pub(crate) fn suspend(&self) {
        self.streaming.store(false, Ordering::Release);
        self.mailbox.suspend();
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.mailbox.close());
    }
}
