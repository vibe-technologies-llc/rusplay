use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::clock::HostClock;

const SMOOTHING_DIVISOR: u64 = 8;

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
    pub stream_formats_received: u64,
    pub frames_dropped: u64,
    pub frames_decoded: u64,
    pub decode_errors: u64,
    pub frames_superseded: u64,
    pub frames_presented: u64,
    pub frames_unpresented: u64,
    pub control_undeliverable: u64,
    pub malformed_datagrams: u64,
    pub rejected_datagrams: u64,
    pub audio_packets_received: u64,
    pub audio_packets_dropped: u64,
    pub audio_packets_late: u64,
    pub audio_packets_lost: u64,
    pub audio_frames_concealed: u64,
    pub audio_frames_recovered: u64,
    pub audio_decode_errors: u64,
    pub audio_underruns: u64,
    pub audio_trims: u64,
    pub audio_overflows: u64,
    pub cursor_updates: u64,
    pub input_packets_sent: u64,
    pub input_events_sent: u64,
    pub input_snapshots_sent: u64,
    pub rumble_commands: u64,
    pub rumble_failures: u64,
    pub gamepad_failures: u64,
    pub bitrate_hints_sent: u64,
    pub bitrate_target_kbps: Option<u32>,
    pub rtt: Option<Duration>,
    pub clock_offset_micros: Option<i64>,
    pub capture_to_receive: Option<Duration>,
    pub capture_to_decode: Option<Duration>,
    pub capture_to_present: Option<Duration>,
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
pub(crate) struct Gauge(AtomicU64);

impl Gauge {
    pub(crate) fn record(&self, sample: Duration) {
        let nanos = u64::try_from(sample.as_nanos()).unwrap_or(u64::MAX).max(1);
        let current = self.0.load(Ordering::Relaxed);
        let smoothed = if current == 0 {
            nanos
        } else {
            current - current / SMOOTHING_DIVISOR + nanos / SMOOTHING_DIVISOR
        };
        self.0.store(smoothed, Ordering::Relaxed);
    }

    fn get(&self) -> Option<Duration> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            nanos => Some(Duration::from_nanos(nanos)),
        }
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
    pub stream_formats_received: Counter,
    pub frames_dropped: Counter,
    pub frames_decoded: Counter,
    pub decode_errors: Counter,
    pub frames_superseded: Counter,
    pub frames_presented: Counter,
    pub frames_unpresented: Counter,
    pub control_undeliverable: Counter,
    pub malformed_datagrams: Counter,
    pub rejected_datagrams: Counter,
    pub audio_packets_received: Counter,
    pub audio_packets_dropped: Counter,
    pub audio_packets_late: Counter,
    pub audio_packets_lost: Counter,
    pub audio_frames_concealed: Counter,
    pub audio_frames_recovered: Counter,
    pub audio_decode_errors: Counter,
    pub audio_underruns: Counter,
    pub audio_trims: Counter,
    pub audio_overflows: Counter,
    pub cursor_updates: Counter,
    pub input_packets_sent: Counter,
    pub input_events_sent: Counter,
    pub input_snapshots_sent: Counter,
    pub rumble_commands: Counter,
    pub rumble_failures: Counter,
    pub gamepad_failures: Counter,
    pub bitrate_hints_sent: Counter,
    bitrate_target_kbps: Counter,
    pub capture_to_receive: Gauge,
    pub capture_to_decode: Gauge,
    pub capture_to_present: Gauge,
    pub clock: HostClock,
    rtt_nanos: Counter,
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
            stream_formats_received: self.stream_formats_received.get(),
            frames_dropped: self.frames_dropped.get(),
            frames_decoded: self.frames_decoded.get(),
            decode_errors: self.decode_errors.get(),
            frames_superseded: self.frames_superseded.get(),
            frames_presented: self.frames_presented.get(),
            frames_unpresented: self.frames_unpresented.get(),
            control_undeliverable: self.control_undeliverable.get(),
            malformed_datagrams: self.malformed_datagrams.get(),
            rejected_datagrams: self.rejected_datagrams.get(),
            audio_packets_received: self.audio_packets_received.get(),
            audio_packets_dropped: self.audio_packets_dropped.get(),
            audio_packets_late: self.audio_packets_late.get(),
            audio_packets_lost: self.audio_packets_lost.get(),
            audio_frames_concealed: self.audio_frames_concealed.get(),
            audio_frames_recovered: self.audio_frames_recovered.get(),
            audio_decode_errors: self.audio_decode_errors.get(),
            audio_underruns: self.audio_underruns.get(),
            audio_trims: self.audio_trims.get(),
            audio_overflows: self.audio_overflows.get(),
            cursor_updates: self.cursor_updates.get(),
            input_packets_sent: self.input_packets_sent.get(),
            input_events_sent: self.input_events_sent.get(),
            input_snapshots_sent: self.input_snapshots_sent.get(),
            rumble_commands: self.rumble_commands.get(),
            rumble_failures: self.rumble_failures.get(),
            gamepad_failures: self.gamepad_failures.get(),
            bitrate_hints_sent: self.bitrate_hints_sent.get(),
            bitrate_target_kbps: match self.bitrate_target_kbps.get() {
                0 => None,
                kbps => Some(u32::try_from(kbps).unwrap_or(u32::MAX)),
            },
            rtt: match self.rtt_nanos.get() {
                0 => None,
                nanos => Some(Duration::from_nanos(nanos)),
            },
            clock_offset_micros: self.clock.offset(),
            capture_to_receive: self.capture_to_receive.get(),
            capture_to_decode: self.capture_to_decode.get(),
            capture_to_present: self.capture_to_present.get(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gauge_is_empty_until_sampled_then_seeds_and_smooths() {
        let gauge = Gauge::default();

        assert_eq!(gauge.get(), None);

        gauge.record(Duration::from_millis(80));
        assert_eq!(gauge.get(), Some(Duration::from_millis(80)));

        gauge.record(Duration::from_millis(160));
        assert_eq!(gauge.get(), Some(Duration::from_millis(90)));
    }
}
