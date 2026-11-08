use std::time::Duration;

use rusplay_client::{ClientStats, StreamDescription};
use rusplay_config::{InputConfig, VideoDecoderConfig};
use rusplay_core::{
    BitDepth, ChromaSiting, ChromaSubsampling, Codec, CodecFormat, ColorRange, ColorSpace,
    PixelFormat, Resolution,
};

use super::{Reading, SectionKind, Sources};
use crate::stats::{ClientLatency, DisplayTiming, EndToEnd, StreamRates};

const ADDRESS: &str = "192.168.1.20:47800";

fn description(configured: PixelFormat) -> StreamDescription {
    let format = PixelFormat {
        chroma: ChromaSubsampling::Yuv444,
        depth: BitDepth::Ten,
    };

    StreamDescription {
        generation: 1,
        codec: Codec::Hevc,
        resolution: Resolution::new(2560, 1440),
        format,
        color: ColorSpace {
            range: ColorRange::Limited,
            ..ColorSpace::default()
        },
        configured: CodecFormat::new(Codec::Hevc, configured),
        siting: ChromaSiting::Left,
    }
}

fn native() -> StreamDescription {
    let described = description(PixelFormat::default());

    StreamDescription {
        configured: described.coded(),
        ..described
    }
}

fn rates() -> StreamRates {
    StreamRates {
        received_fps: 60.0,
        presented_fps: 59.9,
        frames_lost: 0.0,
        frames_recovered: 0.0,
        frames_superseded: 0.0,
        keyframe_requests: 0.0,
        decode_errors: 0.5,
        audio_underruns: 0.0,
        audio_buffer: Duration::from_millis(20),
        latency: Some(ClientLatency {
            decode: Duration::from_millis(3),
            present: Duration::from_millis(1),
            peak: Duration::from_millis(6),
        }),
        end_to_end: None,
        display: None,
        quality: None,
    }
}

struct Fixture {
    decoder: VideoDecoderConfig,
    input: InputConfig,
    stats: ClientStats,
}

impl Fixture {
    fn new() -> Self {
        Self {
            decoder: VideoDecoderConfig::default(),
            input: InputConfig::default(),
            stats: ClientStats::default(),
        }
    }

    fn sources<'a>(
        &'a self,
        format: Option<StreamDescription>,
        rates: Option<&'a StreamRates>,
    ) -> Sources<'a> {
        Sources {
            host: "desk",
            address: ADDRESS.parse().unwrap(),
            decoder: &self.decoder,
            requested_bitrate_kbps: 50_000,
            input: &self.input,
            format,
            stats: rates.map(|_| &self.stats),
            rates,
        }
    }
}

fn value<'a>(readout: &'a super::Readout, kind: SectionKind, label: &str) -> &'a Reading {
    &readout
        .sections
        .iter()
        .find(|section| section.kind == kind)
        .unwrap()
        .rows
        .iter()
        .find(|row| row.label == label)
        .unwrap()
        .value
}

#[test]
fn every_section_and_row_is_in_place_before_the_stream_reports_anything() {
    let fixture = Fixture::new();
    let waiting = fixture.sources(None, None).readout();
    let rates = rates();
    let streaming = fixture.sources(Some(native()), Some(&rates)).readout();

    let shape = |readout: &super::Readout| {
        readout
            .sections
            .iter()
            .map(|section| {
                (
                    section.kind,
                    section.rows.iter().map(|row| row.label).collect::<Vec<_>>(),
                )
            })
            .filter(|(kind, _)| *kind != SectionKind::Quality)
            .collect::<Vec<_>>()
    };

    assert_eq!(shape(&waiting), shape(&streaming));
    assert_eq!(
        *value(&waiting, SectionKind::Stream, "Codec"),
        Reading::Waiting
    );
    assert_eq!(
        *value(&waiting, SectionKind::Latency, "Client total"),
        Reading::Waiting
    );
    assert!(waiting.figures.iter().all(|figure| figure.value.is_none()));
    assert_eq!(
        *value(&waiting, SectionKind::Connection, "Address"),
        Reading::Value(ADDRESS.to_owned())
    );
}

#[test]
fn the_stream_rows_name_the_format_and_a_fallback_adds_the_configured_one() {
    let fixture = Fixture::new();
    let native = fixture.sources(Some(native()), None).readout();
    let fallback = fixture
        .sources(Some(description(PixelFormat::default())), None)
        .readout();

    let configured = fallback
        .sections
        .iter()
        .flat_map(|section| &section.rows)
        .find(|row| row.label == "Configured")
        .unwrap();

    assert_eq!(
        *value(&native, SectionKind::Stream, "Codec"),
        Reading::Value("HEVC · 10-bit 4:4:4".to_owned())
    );
    assert!(matches!(
        value(&native, SectionKind::Stream, "Colour"),
        Reading::Value(colour) if colour.ends_with(" · limited range")
    ));
    assert!(
        !native
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .any(|row| row.label == "Configured")
    );
    assert!(configured.attention);
}

#[test]
fn the_latency_figure_prefers_the_display_then_the_present_then_the_client() {
    let fixture = Fixture::new();
    let client = rates();
    let present = StreamRates {
        end_to_end: Some(EndToEnd {
            receive: None,
            decode: None,
            present: Duration::from_micros(12_340),
        }),
        ..client
    };
    let display = StreamRates {
        display: Some(DisplayTiming {
            submit_to_display: Duration::from_millis(4),
            capture_to_display: Some(Duration::from_micros(16_400)),
            refresh: None,
        }),
        ..present
    };

    let figure = |rates: &StreamRates| {
        let [latency, ..] = fixture.sources(None, Some(rates)).readout().figures;
        (latency.label, latency.value.unwrap())
    };

    assert_eq!(figure(&client), ("client latency", "4.0".to_owned()));
    assert_eq!(figure(&present), ("capture → present", "12.3".to_owned()));
    assert_eq!(figure(&display), ("capture → display", "16.4".to_owned()));
}

#[test]
fn a_rate_above_zero_is_marked_and_missing_feedback_is_said_once_rates_exist() {
    let fixture = Fixture::new();
    let rates = rates();
    let readout = fixture.sources(None, Some(&rates)).readout();

    let row = |label: &str| {
        readout
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .find(|row| row.label == label)
            .unwrap()
    };

    assert!(row("Decode errors").attention);
    assert!(!row("Lost or skipped").attention);
    assert_eq!(
        row("Submit → display").value,
        Reading::Value("no feedback".to_owned())
    );
}

#[test]
fn the_shortcuts_list_every_way_out_that_is_turned_on() {
    let mut fixture = Fixture::new();
    let labels = |fixture: &Fixture| {
        fixture
            .sources(None, None)
            .readout()
            .sections
            .into_iter()
            .find(|section| section.kind == SectionKind::Shortcuts)
            .unwrap()
            .rows
            .into_iter()
            .map(|row| (row.label, row.value))
            .collect::<Vec<_>>()
    };

    let on = labels(&fixture);
    fixture.input.hotkeys = false;
    fixture.input.escape_hold_ms = 0;
    let bare = labels(&fixture);

    assert_eq!(
        on[0],
        ("Release input", Reading::Value("Ctrl+Alt+Shift".to_owned()))
    );
    assert!(on.contains(&("Fullscreen", Reading::Value("Ctrl+Alt+F".to_owned()))));
    assert_eq!(bare.len(), 1);
}
