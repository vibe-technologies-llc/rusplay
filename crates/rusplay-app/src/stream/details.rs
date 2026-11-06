use gpui::{Context, FontWeight, IntoElement, div, prelude::*, px};
use rusplay_client::ClientStream;

use super::StreamView;
use crate::{
    stats::{StreamRates, millis},
    theme,
    widgets::controls::{ButtonStyle, button},
};

const PANEL_WIDTH: f32 = 320.0;

type Rows = Vec<(&'static str, String)>;

impl StreamView {
    pub(super) fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let live = self.live.borrow();
        let format = live.stream.as_ref().and_then(ClientStream::stream_format);
        let stats = live.stream.as_ref().map(ClientStream::stats);
        let mut connection = vec![
            ("Host", self.host.name.clone()),
            ("Address", self.host.address.to_string()),
        ];
        let mut video = Vec::new();
        let (performance, latency) = performance_rows(self.rates);

        if let Some(format) = format {
            video.extend([
                ("Codec", format.codec.to_string()),
                ("Resolution", format.resolution.to_string()),
                ("Bit depth", format!("{}-bit", format.format.depth.bits())),
                ("Chroma", format.format.chroma.to_string()),
                ("Colour primaries", format.color.primaries.to_string()),
                ("Transfer", format.color.transfer.to_string()),
                ("Colour range", format!("{:?}", format.color.range)),
            ]);
            if format.is_fallback() {
                video.push(("Configured format", format.configured.to_string()));
            }
        }
        video.push(("Requested bitrate", bitrate(self.requested_bitrate_kbps)));
        if let Some(stats) = stats {
            connection.push((
                "Round trip",
                stats.rtt.map_or_else(|| "Measuring…".to_owned(), time),
            ));
            video.push((
                "Adaptive target",
                stats
                    .bitrate_target_kbps
                    .map_or_else(|| "Unavailable".to_owned(), bitrate),
            ));
        }

        div()
            .id("connection-details")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(PANEL_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(theme::border())
            .bg(theme::chrome())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .p_3()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Connection details"),
                    )
                    .child(
                        button("close-details", "Close", ButtonStyle::Quiet, true)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_stats(cx))),
                    ),
            )
            .child(
                div()
                    .id("connection-details-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(section("Connection", connection, "Connecting…"))
                    .child(section("Video", video, "Waiting for stream format…"))
                    .child(section("Performance", performance, "Collecting samples…"))
                    .child(section("Latency", latency, "Waiting for timing data…")),
            )
    }
}

fn performance_rows(rates: Option<StreamRates>) -> (Rows, Rows) {
    let mut performance = Vec::new();
    let mut latency = Vec::new();

    if let Some(rates) = rates {
        performance.extend([
            ("Presented", format!("{:.1} fps", rates.presented_fps)),
            ("Received", format!("{:.1} fps", rates.received_fps)),
            ("Lost / skipped", rate(rates.frames_lost)),
            ("Recovered", rate(rates.frames_recovered)),
            ("Superseded", rate(rates.frames_superseded)),
            ("Keyframe requests", rate(rates.keyframe_requests)),
            ("Decode errors", rate(rates.decode_errors)),
            ("Audio underruns", rate(rates.audio_underruns)),
        ]);
        if let Some(end_to_end) = rates.end_to_end {
            latency.push(("Capture → present", time(end_to_end.present)));
            latency.extend(
                end_to_end
                    .receive
                    .map(|value| ("Capture → receive", time(value))),
            );
            latency.extend(
                end_to_end
                    .decode
                    .map(|value| ("Capture → decode", time(value))),
            );
        }
        if let Some(client) = rates.latency {
            latency.extend([
                ("Client total", time(client.total())),
                ("Decode", time(client.decode)),
                ("Present", time(client.present)),
                ("Client peak", time(client.peak)),
            ]);
        }
        if let Some(display) = rates.display {
            latency.push(("Submit → display", time(display.submit_to_display)));
            latency.extend(
                display
                    .capture_to_display
                    .map(|value| ("Capture → display", time(value))),
            );
            latency.push((
                "Display refresh",
                display.refresh.map_or_else(
                    || "Variable".to_owned(),
                    |refresh| format!("{:.1} Hz", 1.0 / refresh.as_secs_f64()),
                ),
            ));
        }
    }

    (performance, latency)
}

fn section(title: &'static str, rows: Rows, empty: &'static str) -> impl IntoElement {
    let is_empty = rows.is_empty();

    div()
        .flex()
        .flex_col()
        .gap_2()
        .mb_5()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::accent_bright())
                .child(title),
        )
        .children(rows.into_iter().map(|(label, value)| {
            div()
                .flex()
                .justify_between()
                .gap_3()
                .text_xs()
                .child(
                    div()
                        .flex_none()
                        .text_color(theme::text_muted())
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_right()
                        .text_color(theme::text())
                        .child(value),
                )
        }))
        .children(is_empty.then(|| div().text_xs().text_color(theme::text_muted()).child(empty)))
}

fn bitrate(kbps: u32) -> String {
    format!("{:.1} Mbps", f64::from(kbps) / 1_000.0)
}

fn rate(value: f64) -> String {
    format!("{value:.1}/s")
}

fn time(value: std::time::Duration) -> String {
    format!("{:.1} ms", millis(value))
}
