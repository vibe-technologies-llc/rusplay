use rusplay_host::{HostStats, JsonLine, StatsPhase};

const EVENT: &str = "stats";

pub fn line(phase: StatsPhase, unix_ms: u128, stats: &HostStats) -> String {
    JsonLine::new(EVENT, phase, unix_ms)
        .number("frames_captured", stats.frames_captured)
        .number("frames_superseded", stats.frames_superseded)
        .number("frames_encoded", stats.frames_encoded)
        .number("frames_repeated", stats.frames_repeated)
        .number("keyframes_sent", stats.keyframes_sent)
        .number("keyframe_requests", stats.keyframe_requests)
        .number("keyframe_requests_ignored", stats.keyframe_requests_ignored)
        .number("recovery_requests", stats.recovery_requests)
        .number("recovery_requests_ignored", stats.recovery_requests_ignored)
        .number("reference_recoveries", stats.reference_recoveries)
        .number("frame_acks", stats.frame_acks)
        .number("frames_dropped", stats.frames_dropped)
        .number("dmabuf_fallbacks", stats.dmabuf_fallbacks)
        .number("dmabuf_import_failures", stats.dmabuf_import_failures)
        .number("handoff_disabled", stats.handoff_disabled)
        .number("frames_sent", stats.frames_sent)
        .number("datagrams_sent", stats.datagrams_sent)
        .number("packets_reported_lost", stats.packets_reported_lost)
        .number("bitrate_changes", stats.bitrate_changes)
        .number("bitrate_hints_clamped", stats.bitrate_hints_clamped)
        .number("bitrate_hints_ignored", stats.bitrate_hints_ignored)
        .optional("bitrate_target_kbps", stats.bitrate_target_kbps)
        .number("keepalives_sent", stats.keepalives_sent)
        .number("sessions_taken_over", stats.sessions_taken_over)
        .number("control_undeliverable", stats.control_undeliverable)
        .number("malformed_datagrams", stats.malformed_datagrams)
        .number("rejected_datagrams", stats.rejected_datagrams)
        .number("audio_packets_sent", stats.audio_packets_sent)
        .number("audio_overruns", stats.audio_overruns)
        .number(
            "microphone_packets_received",
            stats.microphone_packets_received,
        )
        .number(
            "microphone_packets_dropped",
            stats.microphone_packets_dropped,
        )
        .number("microphone_packets_late", stats.microphone_packets_late)
        .number("microphone_packets_lost", stats.microphone_packets_lost)
        .number(
            "microphone_frames_concealed",
            stats.microphone_frames_concealed,
        )
        .number("microphone_decode_errors", stats.microphone_decode_errors)
        .number("microphone_underruns", stats.microphone_underruns)
        .number("microphone_overflows", stats.microphone_overflows)
        .number("cursor_messages_sent", stats.cursor_messages_sent)
        .number("cursor_images_sent", stats.cursor_images_sent)
        .number("input_packets", stats.input_packets)
        .number("input_packets_stale", stats.input_packets_stale)
        .number("input_packets_dropped", stats.input_packets_dropped)
        .number("input_events", stats.input_events)
        .number("input_state_records", stats.input_state_records)
        .number("input_events_repaired", stats.input_events_repaired)
        .number("input_failures", stats.input_failures)
        .number("rumble_sent", stats.rumble_sent)
        .number("rumble_dropped", stats.rumble_dropped)
        .number("thread_priority_failures", stats.thread_priority_failures)
        .optional("rtt_us", stats.rtt.map(|rtt| rtt.as_micros()))
        .finish()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn keys(line: &str) -> Vec<&str> {
        let inner = &line[1..line.len() - 1];
        inner
            .split(',')
            .map(|pair| pair.split(':').next().unwrap().trim_matches('"'))
            .collect()
    }

    #[test]
    fn a_line_is_one_json_object_with_the_phase_and_time() {
        let stats = HostStats {
            frames_sent: 600,
            rtt: Some(Duration::from_micros(1_250)),
            bitrate_target_kbps: Some(40_000),
            ..HostStats::default()
        };

        let line = line(StatsPhase::Interval, 1_700_000_000_123, &stats);

        assert!(
            line.starts_with(r#"{"event":"stats","phase":"interval","unix_ms":1700000000123,"#)
        );
        assert!(line.ends_with('}'));
        assert!(!line.contains('\n'));
        assert!(line.contains(r#""frames_sent":600"#));
        assert!(line.contains(r#""rtt_us":1250"#));
        assert!(line.contains(r#""bitrate_target_kbps":40000"#));
    }

    #[test]
    fn an_unmeasured_value_is_null() {
        let line = line(StatsPhase::Final, 0, &HostStats::default());

        assert!(line.contains(r#""phase":"final""#));
        assert!(line.contains(r#""rtt_us":null"#));
        assert!(line.contains(r#""bitrate_target_kbps":null"#));
    }

    #[test]
    fn every_counter_of_the_snapshot_is_emitted() {
        let line = line(StatsPhase::Final, 0, &HostStats::default());
        let emitted = keys(&line);
        let debug = format!("{:?}", HostStats::default());
        let counters: Vec<&str> = debug
            .trim_start_matches("HostStats { ")
            .trim_end_matches(" }")
            .split(", ")
            .map(|field| field.split(':').next().unwrap())
            .collect();

        assert!(counters.len() > 40);
        for counter in counters {
            let name = if counter == "rtt" { "rtt_us" } else { counter };
            assert!(emitted.contains(&name), "{name} is missing from the line");
        }
    }
}
