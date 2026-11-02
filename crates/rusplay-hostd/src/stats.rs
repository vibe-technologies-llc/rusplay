use std::{
    fmt::Write,
    thread,
    time::{Duration, Instant},
};

use rusplay_host::{
    HostError, HostStats, HostStream, NegotiatedFormat, StatsPhase, StatsSchedule, unix_millis_now,
};

use crate::stats_json;

const POLL: Duration = Duration::from_millis(100);

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
         rumble {}",
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
    println!("{}", stats_json::line(phase, unix_millis_now(), stats));
}

pub fn wait_logging(stream: HostStream, interval_ms: u32, json: bool) -> Result<(), HostError> {
    let mut schedule = StatsSchedule::new(interval_ms, Instant::now());
    let mut format_reported = false;

    while !stream.is_finished() {
        thread::sleep(POLL);
        if !format_reported && let Some(format) = stream.format() {
            format_reported = true;
            eprintln!("rusplay-hostd: {}", format_line(format));
        }
        if schedule.take_due(Instant::now()) {
            let stats = stream.stats();
            eprintln!("rusplay-hostd: stats {}", summary(&stats));
            if json {
                emit_json(StatsPhase::Interval, &stats);
            }
        }
    }

    let last = stream.stats();
    let result = stream.wait();
    eprintln!("rusplay-hostd: session summary {}", summary(&last));
    if json {
        emit_json(StatsPhase::Final, &last);
    }

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
