use std::{fmt::Write, time::Instant};

use rusplay_host::{
    HostStats, NegotiatedFormat, StatsPhase, StatsSchedule, StreamSnapshot, host_stats_json_line,
    unix_millis_now,
};

const JSON_EVENT: &str = "stats";

pub fn summary(stats: &HostStats) -> String {
    let mut line = String::new();
    let rtt = stats.rtt.map_or_else(
        || "n/a".to_owned(),
        |rtt| format!("{:.2} ms", rtt.as_secs_f64() * 1_000.0),
    );

    write!(
        line,
        "rtt {rtt}, frames captured {} encoded {} sent {} dropped {} repeated {}, dmabuf fallbacks {} import failures {} handoff off {}, \
         keyframes {} requested {}, datagrams {}, reported lost {}, undeliverable {}, \
         malformed {} rejected {}, audio {} overruns {}, microphone {} lost {} underruns {}, cursor {}, input packets {} events {} dropped {}, \
         rumble {} feedback {}, virtual resizes {} display updates {} coalesced {}",
        stats.frames_captured,
        stats.frames_encoded,
        stats.frames_sent,
        stats.frames_dropped,
        stats.frames_repeated,
        stats.dmabuf_fallbacks,
        stats.dmabuf_import_failures,
        stats.handoff_disabled,
        stats.keyframes_sent,
        stats.keyframe_requests,
        stats.datagrams_sent,
        stats.packets_reported_lost,
        stats.control_undeliverable,
        stats.malformed_datagrams,
        stats.rejected_datagrams,
        stats.audio_packets_sent,
        stats.audio_overruns,
        stats.microphone_packets_received,
        stats.microphone_packets_lost,
        stats.microphone_underruns,
        stats.cursor_messages_sent,
        stats.input_packets,
        stats.input_events,
        stats.input_packets_dropped,
        stats.rumble_sent,
        stats.feedback_sent,
        stats.virtual_resizes,
        stats.display_updates_received,
        stats.display_updates_coalesced,
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

fn emit_json(phase: StatsPhase, stats: &HostStats) {
    println!(
        "{}",
        host_stats_json_line(JSON_EVENT, phase, unix_millis_now(), stats)
    );
}

#[derive(Debug)]
pub struct StreamLog {
    schedule: StatsSchedule,
    json: bool,
    format_reported: bool,
}

impl StreamLog {
    pub fn new(interval_ms: u32, json: bool, now: Instant) -> Self {
        Self {
            schedule: StatsSchedule::new(interval_ms, now),
            json,
            format_reported: false,
        }
    }

    pub fn observe(&mut self, snapshot: &StreamSnapshot, now: Instant) {
        if !self.format_reported
            && let Some(format) = snapshot.format
        {
            self.format_reported = true;
            eprintln!("rusplay-hostd: {}", format_line(format));
        }
        if self.schedule.take_due(now) {
            eprintln!("rusplay-hostd: stats {}", summary(&snapshot.stats));
            if self.json {
                emit_json(StatsPhase::Interval, &snapshot.stats);
            }
        }
    }

    pub fn finish(&self, stats: &HostStats) {
        eprintln!("rusplay-hostd: session summary {}", summary(stats));
        if self.json {
            emit_json(StatsPhase::Final, stats);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
