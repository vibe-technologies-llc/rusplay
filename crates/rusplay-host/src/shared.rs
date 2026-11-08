use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use rusplay_capture::OutputRequest;

use crate::{
    capture::Captured, clock::StreamClock, link::ClientLink, mailbox::Mailbox,
    negotiate::Negotiation, stats::Counters,
};

#[derive(Debug)]
pub(crate) struct Shared {
    stop: AtomicBool,
    streaming: AtomicBool,
    quality_requested: AtomicBool,
    pub client: Arc<ClientLink>,
    pub clock: StreamClock,
    pub counters: Counters,
    pub mailbox: Mailbox<Captured>,
    pub negotiation: Negotiation,
    output: Mutex<Option<OutputRequest>>,
}

impl Shared {
    pub(crate) fn new(client: SocketAddr) -> Self {
        Self {
            stop: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
            client: Arc::new(ClientLink::new(client)),
            clock: StreamClock::start(),
            counters: Counters::default(),
            mailbox: Mailbox::new(),
            negotiation: Negotiation::default(),
            quality_requested: AtomicBool::new(false),
            output: Mutex::new(None),
        }
    }

    pub(crate) fn set_output(&self, request: OutputRequest) {
        *self.output.lock() = Some(request);
    }

    pub(crate) fn output(&self) -> Option<OutputRequest> {
        *self.output.lock()
    }

    pub(crate) fn request_quality(&self, enabled: bool) {
        self.quality_requested.store(enabled, Ordering::Release);
    }

    pub(crate) fn quality_requested(&self) -> bool {
        self.quality_requested.load(Ordering::Acquire)
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

pub(crate) struct StopOnDrop(pub Arc<Shared>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}
