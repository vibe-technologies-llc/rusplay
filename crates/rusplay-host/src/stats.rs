use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

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
    pub bitrate_hints_clamped: u64,
    pub bitrate_hints_ignored: u64,
    pub bitrate_target_kbps: Option<u32>,
    pub keepalives_sent: u64,
    pub sessions_taken_over: u64,
    pub control_undeliverable: u64,
    pub malformed_datagrams: u64,
    pub rejected_datagrams: u64,
    pub audio_packets_sent: u64,
    pub audio_overruns: u64,
    pub cursor_messages_sent: u64,
    pub cursor_images_sent: u64,
    pub input_packets: u64,
    pub input_packets_stale: u64,
    pub input_events: u64,
    pub input_state_records: u64,
    pub input_events_repaired: u64,
    pub input_failures: u64,
    pub rumble_sent: u64,
    pub rumble_dropped: u64,
    pub rtt: Option<Duration>,
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
    pub bitrate_hints_clamped: Counter,
    pub bitrate_hints_ignored: Counter,
    bitrate_target_kbps: Counter,
    pub keepalives_sent: Counter,
    pub sessions_taken_over: Counter,
    pub control_undeliverable: Counter,
    pub malformed_datagrams: Counter,
    pub rejected_datagrams: Counter,
    pub audio_packets_sent: Counter,
    pub audio_overruns: Counter,
    pub cursor_messages_sent: Counter,
    pub cursor_images_sent: Counter,
    pub input_packets: Counter,
    pub input_packets_stale: Counter,
    pub input_events: Counter,
    pub input_state_records: Counter,
    pub input_events_repaired: Counter,
    pub input_failures: Counter,
    pub rumble_sent: Counter,
    pub rumble_dropped: Counter,
    rtt_nanos: Counter,
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
            bitrate_hints_clamped: self.bitrate_hints_clamped.get(),
            bitrate_hints_ignored: self.bitrate_hints_ignored.get(),
            bitrate_target_kbps: match self.bitrate_target_kbps.get() {
                0 => None,
                kbps => Some(u32::try_from(kbps).unwrap_or(u32::MAX)),
            },
            keepalives_sent: self.keepalives_sent.get(),
            sessions_taken_over: self.sessions_taken_over.get(),
            control_undeliverable: self.control_undeliverable.get(),
            malformed_datagrams: self.malformed_datagrams.get(),
            rejected_datagrams: self.rejected_datagrams.get(),
            audio_packets_sent: self.audio_packets_sent.get(),
            audio_overruns: self.audio_overruns.get(),
            cursor_messages_sent: self.cursor_messages_sent.get(),
            cursor_images_sent: self.cursor_images_sent.get(),
            input_packets: self.input_packets.get(),
            input_packets_stale: self.input_packets_stale.get(),
            input_events: self.input_events.get(),
            input_state_records: self.input_state_records.get(),
            input_events_repaired: self.input_events_repaired.get(),
            input_failures: self.input_failures.get(),
            rumble_sent: self.rumble_sent.get(),
            rumble_dropped: self.rumble_dropped.get(),
            rtt: match self.rtt_nanos.get() {
                0 => None,
                nanos => Some(Duration::from_nanos(nanos)),
            },
        }
    }

    pub(crate) fn set_bitrate_target(&self, kbps: u32) {
        self.bitrate_target_kbps.set(u64::from(kbps));
    }

    pub(crate) fn set_rtt(&self, rtt: Option<Duration>) {
        let nanos = rtt.map_or(0, |rtt| {
            u64::try_from(rtt.as_nanos()).unwrap_or(u64::MAX).max(1)
        });
        self.rtt_nanos.set(nanos);
    }
}
