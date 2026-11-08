use std::net::SocketAddr;

use rusplay_client::{ClientStats, StreamDescription};
use rusplay_config::{InputConfig, VideoDecoderConfig};
use rusplay_core::{Codec, ColorRange};

use super::chord_label;
use crate::stats::{
    StreamRates, bitrate_text, fps_text, millis, millis_text, quality_rows, rate_text,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    Value(String),
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: &'static str,
    pub value: Reading,
    pub attention: bool,
}

impl Row {
    fn value(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: Reading::Value(value.into()),
            attention: false,
        }
    }

    fn waiting(label: &'static str) -> Self {
        Self {
            label,
            value: Reading::Waiting,
            attention: false,
        }
    }

    fn maybe(label: &'static str, value: Option<String>) -> Self {
        value.map_or_else(|| Self::waiting(label), |value| Self::value(label, value))
    }

    fn rate(label: &'static str, value: Option<f64>) -> Self {
        Self {
            attention: value.is_some_and(|value| value > 0.0),
            ..Self::maybe(label, value.map(rate_text))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Connection,
    Stream,
    Latency,
    Delivery,
    Audio,
    Quality,
    Shortcuts,
}

impl SectionKind {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Connection => "Connection",
            Self::Stream => "Stream",
            Self::Latency => "Latency",
            Self::Delivery => "Delivery",
            Self::Audio => "Audio",
            Self::Quality => "Encode quality",
            Self::Shortcuts => "Shortcuts",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub kind: SectionKind,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Figure {
    pub label: &'static str,
    pub value: Option<String>,
    pub unit: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readout {
    pub figures: [Figure; 3],
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    pub host: &'a str,
    pub address: SocketAddr,
    pub decoder: &'a VideoDecoderConfig,
    pub requested_bitrate_kbps: u32,
    pub input: &'a InputConfig,
    pub format: Option<StreamDescription>,
    pub stats: Option<&'a ClientStats>,
    pub rates: Option<&'a StreamRates>,
}

impl Sources<'_> {
    pub fn readout(&self) -> Readout {
        Readout {
            figures: [
                self.latency_figure(),
                Figure {
                    label: "presented",
                    value: self
                        .rates
                        .map(|rates| format!("{:.1}", rates.presented_fps)),
                    unit: "fps",
                },
                Figure {
                    label: "round trip",
                    value: self
                        .stats
                        .and_then(|stats| stats.rtt)
                        .map(|rtt| format!("{:.1}", millis(rtt))),
                    unit: "ms",
                },
            ],
            sections: vec![
                self.connection(),
                self.stream(),
                self.latency(),
                self.delivery(),
                self.audio(),
                self.quality(),
                self.shortcuts(),
            ],
        }
    }

    fn latency_figure(&self) -> Figure {
        let display = self
            .rates
            .and_then(|rates| rates.display)
            .and_then(|display| display.capture_to_display)
            .map(|time| ("capture → display", time));
        let present = self
            .rates
            .and_then(|rates| rates.end_to_end)
            .map(|end_to_end| ("capture → present", end_to_end.present));
        let client = self
            .rates
            .and_then(|rates| rates.latency)
            .map(|latency| ("client latency", latency.total()));

        match display.or(present).or(client) {
            Some((label, time)) => Figure {
                label,
                value: Some(format!("{:.1}", millis(time))),
                unit: "ms",
            },
            None => Figure {
                label: "latency",
                value: None,
                unit: "ms",
            },
        }
    }

    fn connection(&self) -> Section {
        Section {
            kind: SectionKind::Connection,
            rows: vec![
                Row::value("Host", self.host),
                Row::value("Address", self.address.to_string()),
                Row::maybe(
                    "Round trip",
                    self.stats.and_then(|stats| stats.rtt).map(millis_text),
                ),
            ],
        }
    }

    fn stream(&self) -> Section {
        let format = self.format;
        let mut rows = vec![
            Row::maybe(
                "Codec",
                format.map(|format| {
                    format!(
                        "{} · {}-bit {}",
                        codec_label(format.codec),
                        format.format.depth.bits(),
                        format.format.chroma
                    )
                }),
            ),
            Row::maybe(
                "Resolution",
                format.map(|format| format.resolution.to_string()),
            ),
            Row::maybe(
                "Colour",
                format.map(|format| {
                    format!(
                        "{} · {} · {}",
                        format.color.primaries,
                        format.color.transfer,
                        range_label(format.color.range)
                    )
                }),
            ),
        ];
        if let Some(format) = format.filter(StreamDescription::is_fallback) {
            rows.push(Row {
                attention: true,
                ..Row::value("Configured", format.configured.to_string())
            });
        }
        rows.extend([
            Row::value("Decoder", decoder_label(self.decoder)),
            Row::value(
                "Requested bitrate",
                bitrate_text(self.requested_bitrate_kbps),
            ),
            Row::maybe(
                "Adaptive target",
                self.stats.map(|stats| {
                    stats
                        .bitrate_target_kbps
                        .map_or_else(|| "unavailable".to_owned(), bitrate_text)
                }),
            ),
        ]);

        Section {
            kind: SectionKind::Stream,
            rows,
        }
    }

    fn latency(&self) -> Section {
        let end_to_end = self.rates.and_then(|rates| rates.end_to_end);
        let client = self.rates.and_then(|rates| rates.latency);
        let display = self.rates.and_then(|rates| rates.display);
        let feedback = |value: Option<String>| match (self.rates, value) {
            (_, Some(value)) => Some(value),
            (Some(_), None) => Some("no feedback".to_owned()),
            (None, None) => None,
        };

        Section {
            kind: SectionKind::Latency,
            rows: vec![
                Row::maybe(
                    "Capture → receive",
                    end_to_end
                        .and_then(|end_to_end| end_to_end.receive)
                        .map(millis_text),
                ),
                Row::maybe(
                    "Capture → decode",
                    end_to_end
                        .and_then(|end_to_end| end_to_end.decode)
                        .map(millis_text),
                ),
                Row::maybe(
                    "Capture → present",
                    end_to_end.map(|end_to_end| millis_text(end_to_end.present)),
                ),
                Row::maybe(
                    "Capture → display",
                    feedback(
                        display
                            .and_then(|display| display.capture_to_display)
                            .map(millis_text),
                    ),
                ),
                Row::maybe(
                    "Client decode",
                    client.map(|client| millis_text(client.decode)),
                ),
                Row::maybe(
                    "Client present",
                    client.map(|client| millis_text(client.present)),
                ),
                Row::maybe(
                    "Client total",
                    client.map(|client| millis_text(client.total())),
                ),
                Row::maybe("Client peak", client.map(|client| millis_text(client.peak))),
                Row::maybe(
                    "Submit → display",
                    feedback(display.map(|display| millis_text(display.submit_to_display))),
                ),
                Row::maybe(
                    "Display refresh",
                    feedback(display.map(|display| {
                        display.refresh.map_or_else(
                            || "variable".to_owned(),
                            |refresh| format!("{:.1} Hz", 1.0 / refresh.as_secs_f64()),
                        )
                    })),
                ),
            ],
        }
    }

    fn delivery(&self) -> Section {
        let rates = self.rates;

        Section {
            kind: SectionKind::Delivery,
            rows: vec![
                Row::maybe(
                    "Presented",
                    rates.map(|rates| fps_text(rates.presented_fps)),
                ),
                Row::maybe("Received", rates.map(|rates| fps_text(rates.received_fps))),
                Row::rate("Lost or skipped", rates.map(|rates| rates.frames_lost)),
                Row::maybe(
                    "Recovered",
                    rates.map(|rates| rate_text(rates.frames_recovered)),
                ),
                Row::maybe(
                    "Superseded",
                    rates.map(|rates| rate_text(rates.frames_superseded)),
                ),
                Row::maybe(
                    "Keyframe requests",
                    rates.map(|rates| rate_text(rates.keyframe_requests)),
                ),
                Row::rate("Decode errors", rates.map(|rates| rates.decode_errors)),
            ],
        }
    }

    fn audio(&self) -> Section {
        Section {
            kind: SectionKind::Audio,
            rows: vec![
                Row::rate("Underruns", self.rates.map(|rates| rates.audio_underruns)),
                Row::maybe(
                    "Jitter buffer",
                    self.rates.map(|rates| millis_text(rates.audio_buffer)),
                ),
            ],
        }
    }

    fn quality(&self) -> Section {
        let rows = self.stats.map_or_else(
            || vec![Row::waiting("Status")],
            |stats| {
                quality_rows(stats)
                    .into_iter()
                    .map(|(label, value)| Row::value(label, value))
                    .collect()
            },
        );

        Section {
            kind: SectionKind::Quality,
            rows,
        }
    }

    fn shortcuts(&self) -> Section {
        let input = self.input;
        let mut rows = vec![Row::value(
            "Release input",
            chord_label(input.release_chord),
        )];

        if input.escape_hold_ms != 0 {
            rows.push(Row::value("Release and leave fullscreen", "hold Esc"));
        }
        if input.hotkeys {
            let chord = chord_label(input.hotkey_chord);
            rows.extend([
                Row::value(
                    "Fullscreen",
                    format!("{chord}+{:?}", input.hotkey_fullscreen),
                ),
                Row::value("Details", format!("{chord}+{:?}", input.hotkey_stats)),
                Row::value(
                    "Disconnect",
                    format!("{chord}+{:?}", input.hotkey_disconnect),
                ),
                Row::value(
                    "Pointer mode",
                    format!("{chord}+{:?}", input.hotkey_pointer_mode),
                ),
            ]);
        }

        Section {
            kind: SectionKind::Shortcuts,
            rows,
        }
    }
}

const fn codec_label(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::Hevc => "HEVC",
        Codec::Av1 => "AV1",
    }
}

const fn range_label(range: ColorRange) -> &'static str {
    match range {
        ColorRange::Full => "full range",
        ColorRange::Limited => "limited range",
    }
}

const fn decoder_label(decoder: &VideoDecoderConfig) -> &'static str {
    match decoder {
        VideoDecoderConfig::Vaapi(config) if config.copy_output => "VAAPI · CPU copy",
        VideoDecoderConfig::Vaapi(_) => "VAAPI · GPU surfaces",
        VideoDecoderConfig::Nvdec(_) => "NVDEC · CPU upload",
        VideoDecoderConfig::Software(_) => "Software · CPU upload",
    }
}

#[cfg(test)]
mod tests;
