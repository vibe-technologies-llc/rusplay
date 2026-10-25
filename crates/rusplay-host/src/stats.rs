use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostStats {
    pub frames_captured: u64,
    pub frames_superseded: u64,
    pub frames_encoded: u64,
    pub frames_repeated: u64,
    pub keyframes_sent: u64,
    pub keyframe_requests: u64,
    pub keyframe_requests_ignored: u64,
    pub frames_dropped: u64,
    pub frames_sent: u64,
    pub datagrams_sent: u64,
    pub packets_reported_lost: u64,
    pub bitrate_changes: u64,
    pub keepalives_sent: u64,
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
    pub frames_captured: Counter,
    pub frames_superseded: Counter,
    pub frames_encoded: Counter,
    pub frames_repeated: Counter,
    pub keyframes_sent: Counter,
    pub keyframe_requests: Counter,
    pub keyframe_requests_ignored: Counter,
    pub frames_dropped: Counter,
    pub frames_sent: Counter,
    pub datagrams_sent: Counter,
    pub packets_reported_lost: Counter,
    pub bitrate_changes: Counter,
    pub keepalives_sent: Counter,
    pub control_undeliverable: Counter,
    pub malformed_datagrams: Counter,
    pub rejected_datagrams: Counter,
}

impl Counters {
    pub(crate) fn snapshot(&self) -> HostStats {
        HostStats {
            frames_captured: self.frames_captured.get(),
            frames_superseded: self.frames_superseded.get(),
            frames_encoded: self.frames_encoded.get(),
            frames_repeated: self.frames_repeated.get(),
            keyframes_sent: self.keyframes_sent.get(),
            keyframe_requests: self.keyframe_requests.get(),
            keyframe_requests_ignored: self.keyframe_requests_ignored.get(),
            frames_dropped: self.frames_dropped.get(),
            frames_sent: self.frames_sent.get(),
            datagrams_sent: self.datagrams_sent.get(),
            packets_reported_lost: self.packets_reported_lost.get(),
            bitrate_changes: self.bitrate_changes.get(),
            keepalives_sent: self.keepalives_sent.get(),
            control_undeliverable: self.control_undeliverable.get(),
            malformed_datagrams: self.malformed_datagrams.get(),
            rejected_datagrams: self.rejected_datagrams.get(),
        }
    }
}
