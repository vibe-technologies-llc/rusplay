use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClientStats {
    pub frames_received: u64,
    pub frames_recovered: u64,
    pub frames_lost: u64,
    pub packets_missing: u64,
    pub loss_reports_sent: u64,
    pub loss_reports_dropped: u64,
    pub keyframe_requests_sent: u64,
    pub frames_skipped: u64,
    pub frames_dropped: u64,
    pub frames_decoded: u64,
    pub decode_errors: u64,
    pub frames_superseded: u64,
    pub frames_presented: u64,
    pub frames_unpresented: u64,
    pub control_undeliverable: u64,
    pub malformed_datagrams: u64,
    pub rejected_datagrams: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Counter(AtomicU64);

impl Counter {
    pub(crate) fn add(&self, count: u64) {
        self.0.fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn increment(&self) {
        self.add(1);
    }

    pub(crate) fn set(&self, value: u64) {
        self.0.store(value, Ordering::Relaxed);
    }

    fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub frames_received: Counter,
    pub frames_recovered: Counter,
    pub frames_lost: Counter,
    pub packets_missing: Counter,
    pub loss_reports_sent: Counter,
    pub loss_reports_dropped: Counter,
    pub keyframe_requests_sent: Counter,
    pub frames_skipped: Counter,
    pub frames_dropped: Counter,
    pub frames_decoded: Counter,
    pub decode_errors: Counter,
    pub frames_superseded: Counter,
    pub frames_presented: Counter,
    pub frames_unpresented: Counter,
    pub control_undeliverable: Counter,
    pub malformed_datagrams: Counter,
    pub rejected_datagrams: Counter,
}

impl Counters {
    pub(crate) fn snapshot(&self) -> ClientStats {
        ClientStats {
            frames_received: self.frames_received.get(),
            frames_recovered: self.frames_recovered.get(),
            frames_lost: self.frames_lost.get(),
            packets_missing: self.packets_missing.get(),
            loss_reports_sent: self.loss_reports_sent.get(),
            loss_reports_dropped: self.loss_reports_dropped.get(),
            keyframe_requests_sent: self.keyframe_requests_sent.get(),
            frames_skipped: self.frames_skipped.get(),
            frames_dropped: self.frames_dropped.get(),
            frames_decoded: self.frames_decoded.get(),
            decode_errors: self.decode_errors.get(),
            frames_superseded: self.frames_superseded.get(),
            frames_presented: self.frames_presented.get(),
            frames_unpresented: self.frames_unpresented.get(),
            control_undeliverable: self.control_undeliverable.get(),
            malformed_datagrams: self.malformed_datagrams.get(),
            rejected_datagrams: self.rejected_datagrams.get(),
        }
    }
}
