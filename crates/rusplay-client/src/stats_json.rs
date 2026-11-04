use rusplay_core::{JsonLine, StatsPhase};

use crate::stats::ClientStats;

const EVENT: &str = "client_stats";

pub fn stats_json_line(phase: StatsPhase, unix_ms: u128, stats: &ClientStats) -> String {
    JsonLine::new(EVENT, phase, unix_ms)
        .number("frames_received", stats.frames_received)
        .number("frames_recovered", stats.frames_recovered)
        .number("frames_lost", stats.frames_lost)
        .number("packets_missing", stats.packets_missing)
        .number("loss_reports_sent", stats.loss_reports_sent)
        .number("loss_reports_dropped", stats.loss_reports_dropped)
        .number("keyframe_requests_sent", stats.keyframe_requests_sent)
        .number("recovery_requests_sent", stats.recovery_requests_sent)
        .number("reference_recoveries", stats.reference_recoveries)
        .number("frame_acks_sent", stats.frame_acks_sent)
        .number("frames_skipped", stats.frames_skipped)
        .number("stream_formats_received", stats.stream_formats_received)
        .number("frames_dropped", stats.frames_dropped)
        .number("frames_decoded", stats.frames_decoded)
        .number("decode_errors", stats.decode_errors)
        .number("frames_superseded", stats.frames_superseded)
        .number("frames_presented", stats.frames_presented)
        .number("frames_unpresented", stats.frames_unpresented)
        .number("control_undeliverable", stats.control_undeliverable)
        .number("malformed_datagrams", stats.malformed_datagrams)
        .number("rejected_datagrams", stats.rejected_datagrams)
        .number("audio_packets_received", stats.audio_packets_received)
        .number("audio_packets_dropped", stats.audio_packets_dropped)
        .number("audio_packets_late", stats.audio_packets_late)
        .number("audio_packets_lost", stats.audio_packets_lost)
        .number("audio_frames_concealed", stats.audio_frames_concealed)
        .number("audio_frames_recovered", stats.audio_frames_recovered)
        .number("audio_decode_errors", stats.audio_decode_errors)
        .number("audio_underruns", stats.audio_underruns)
        .number("audio_trims", stats.audio_trims)
        .number("audio_overflows", stats.audio_overflows)
        .number("microphone_packets_sent", stats.microphone_packets_sent)
        .number("microphone_overruns", stats.microphone_overruns)
        .number("cursor_updates", stats.cursor_updates)
        .number("input_packets_sent", stats.input_packets_sent)
        .number("input_events_sent", stats.input_events_sent)
        .number("input_snapshots_sent", stats.input_snapshots_sent)
        .number("rumble_commands", stats.rumble_commands)
        .number("rumble_failures", stats.rumble_failures)
        .number("feedback_commands", stats.feedback_commands)
        .number("feedback_dropped", stats.feedback_dropped)
        .number("feedback_failures", stats.feedback_failures)
        .number("gamepad_failures", stats.gamepad_failures)
        .number("thread_priority_failures", stats.thread_priority_failures)
        .number("bitrate_hints_sent", stats.bitrate_hints_sent)
        .optional("bitrate_target_kbps", stats.bitrate_target_kbps)
        .optional("rtt_us", stats.rtt.map(|rtt| rtt.as_micros()))
        .optional("clock_offset_us", stats.clock_offset_micros)
        .optional(
            "capture_to_receive_us",
            stats.capture_to_receive.map(|latency| latency.as_micros()),
        )
        .optional(
            "capture_to_decode_us",
            stats.capture_to_decode.map(|latency| latency.as_micros()),
        )
        .optional(
            "capture_to_present_us",
            stats.capture_to_present.map(|latency| latency.as_micros()),
        )
        .finish()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn keys(line: &str) -> Vec<&str> {
        line[1..line.len() - 1]
            .split(',')
            .map(|pair| pair.split(':').next().unwrap().trim_matches('"'))
            .collect()
    }

    #[test]
    fn a_line_is_one_json_object_with_the_end_to_end_latency() {
        let stats = ClientStats {
            frames_presented: 600,
            rtt: Some(Duration::from_micros(1_250)),
            capture_to_present: Some(Duration::from_micros(18_400)),
            clock_offset_micros: Some(-35),
            ..ClientStats::default()
        };

        let line = stats_json_line(StatsPhase::Interval, 1_700_000_000_123, &stats);

        assert!(
            line.starts_with(
                r#"{"event":"client_stats","phase":"interval","unix_ms":1700000000123,"#
            )
        );
        assert!(line.ends_with('}'));
        assert!(!line.contains('\n'));
        assert!(line.contains(r#""frames_presented":600"#));
        assert!(line.contains(r#""rtt_us":1250"#));
        assert!(line.contains(r#""capture_to_present_us":18400"#));
        assert!(line.contains(r#""clock_offset_us":-35"#));
    }

    #[test]
    fn an_unmeasured_value_is_null() {
        let line = stats_json_line(StatsPhase::Final, 0, &ClientStats::default());

        assert!(line.contains(r#""phase":"final""#));
        assert!(line.contains(r#""rtt_us":null"#));
        assert!(line.contains(r#""capture_to_present_us":null"#));
        assert!(line.contains(r#""bitrate_target_kbps":null"#));
    }

    #[test]
    fn every_counter_of_the_snapshot_is_emitted() {
        let line = stats_json_line(StatsPhase::Final, 0, &ClientStats::default());
        let emitted = keys(&line);
        let debug = format!("{:?}", ClientStats::default());
        let counters: Vec<&str> = debug
            .trim_start_matches("ClientStats { ")
            .trim_end_matches(" }")
            .split(", ")
            .map(|field| field.split(':').next().unwrap())
            .collect();

        assert!(counters.len() > 40);
        for counter in counters {
            let name = match counter {
                "rtt" => "rtt_us".to_owned(),
                "clock_offset_micros" => "clock_offset_us".to_owned(),
                other if other.starts_with("capture_to_") => format!("{other}_us"),
                other => other.to_owned(),
            };
            assert!(
                emitted.contains(&name.as_str()),
                "{name} is missing from the line"
            );
        }
    }
}
