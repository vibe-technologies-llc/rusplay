use std::{
    fmt::Write,
    thread,
    time::{Duration, Instant},
};

use rusplay_host::{HostError, HostStats, HostStream, NegotiatedFormat};

const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Schedule {
    interval: Option<Duration>,
    next: Instant,
}

impl Schedule {
    fn new(interval_ms: u32, now: Instant) -> Self {
        let interval = (interval_ms > 0).then(|| Duration::from_millis(u64::from(interval_ms)));

        Self {
            interval,
            next: now + interval.unwrap_or_default(),
        }
    }

    fn take_due(&mut self, now: Instant) -> bool {
        let Some(interval) = self.interval else {
            return false;
        };
        if now < self.next {
            return false;
        }

        self.next = now + interval;
        true
    }
}

pub fn summary(stats: &HostStats) -> String {
    let mut line = String::new();
    let rtt = stats.rtt.map_or_else(
        || "n/a".to_owned(),
        |rtt| format!("{:.2} ms", rtt.as_secs_f64() * 1_000.0),
    );

    write!(
        line,
        "rtt {rtt}, frames captured {} encoded {} sent {} dropped {} repeated {}, \
         keyframes {} requested {}, datagrams {}, reported lost {}, undeliverable {}, \
         malformed {} rejected {}, audio {} overruns {}, cursor {}, input packets {} events {}, \
         rumble {}",
        stats.frames_captured,
        stats.frames_encoded,
        stats.frames_sent,
        stats.frames_dropped,
        stats.frames_repeated,
        stats.keyframes_sent,
        stats.keyframe_requests,
        stats.datagrams_sent,
        stats.packets_reported_lost,
        stats.control_undeliverable,
        stats.malformed_datagrams,
        stats.rejected_datagrams,
        stats.audio_packets_sent,
        stats.audio_overruns,
        stats.cursor_messages_sent,
        stats.input_packets,
        stats.input_events,
        stats.rumble_sent,
    )
    .ok();

    line
}

fn format_line(format: NegotiatedFormat) -> String {
    if format.is_fallback() {
        format!(
            "streaming {} instead of the configured {}, which the encoder or client lacks",
            format.chosen, format.configured
        )
    } else {
        format!("streaming {}", format.chosen)
    }
}

pub fn wait_logging(stream: HostStream, interval_ms: u32) -> Result<(), HostError> {
    let mut schedule = Schedule::new(interval_ms, Instant::now());
    let mut format_reported = false;

    while !stream.is_finished() {
        thread::sleep(POLL);
        if !format_reported && let Some(format) = stream.format() {
            format_reported = true;
            eprintln!("rusplay-hostd: {}", format_line(format));
        }
        if schedule.take_due(Instant::now()) {
            eprintln!("rusplay-hostd: stats {}", summary(&stream.stats()));
        }
    }

    let last = stream.stats();
    let result = stream.wait();
    eprintln!("rusplay-hostd: session summary {}", summary(&last));

    result
}

#[cfg(test)]
mod tests {
    use rusplay_host::CodecFormat;

    use super::*;

    #[test]
    fn a_fallback_names_both_formats() {
        let mut formats = CodecFormat::all();
        let configured = formats.next().unwrap();
        let chosen = formats.next().unwrap();

        let kept = format_line(NegotiatedFormat {
            configured,
            chosen: configured,
        });
        let fell_back = format_line(NegotiatedFormat { configured, chosen });

        assert_eq!(kept, "streaming h264 4:2:0 8-bit");
        assert_eq!(
            fell_back,
            "streaming h264 4:2:0 10-bit instead of the configured h264 4:2:0 8-bit, which the encoder or client lacks"
        );
    }

    #[test]
    fn zero_interval_never_comes_due() {
        let start = Instant::now();
        let mut schedule = Schedule::new(0, start);

        assert!(!schedule.take_due(start + Duration::from_secs(3_600)));
    }

    #[test]
    fn interval_comes_due_once_per_period() {
        let start = Instant::now();
        let mut schedule = Schedule::new(1_000, start);

        assert!(!schedule.take_due(start + Duration::from_millis(999)));
        assert!(schedule.take_due(start + Duration::from_millis(1_000)));
        assert!(!schedule.take_due(start + Duration::from_millis(1_500)));
        assert!(schedule.take_due(start + Duration::from_millis(2_000)));
    }

    #[test]
    fn summary_reports_counters_and_rtt() {
        let stats = HostStats {
            frames_sent: 600,
            keyframes_sent: 2,
            rtt: Some(Duration::from_micros(1_250)),
            ..HostStats::default()
        };

        let line = summary(&stats);

        assert!(line.starts_with("rtt 1.25 ms,"));
        assert!(line.contains("sent 600"));
        assert!(line.contains("keyframes 2 requested 0"));
    }

    #[test]
    fn summary_marks_an_unmeasured_rtt() {
        assert!(summary(&HostStats::default()).starts_with("rtt n/a,"));
    }
}
