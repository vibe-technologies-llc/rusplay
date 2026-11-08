use gpui::{AnyElement, Context, FontWeight, IntoElement, div, prelude::*, px};
use rusplay_client::ClientStream;

use super::{
    StreamView,
    readout::{Figure, Reading, Row, Section, SectionKind, Sources},
};
use crate::{
    theme,
    widgets::controls::{ButtonStyle, button},
};

const PANEL_WIDTH: f32 = 300.0;
const WAITING: &str = "waiting";
const NO_FIGURE: &str = "\u{2014}";

impl StreamView {
    pub(super) fn render_details(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let live = self.live.borrow();
        let stats = live.stream.as_ref().map(ClientStream::stats);
        let measuring = live
            .stream
            .as_ref()
            .is_some_and(ClientStream::quality_metrics);
        let readout = Sources {
            host: &self.host.name,
            address: self.host.address,
            decoder: &self.decoder,
            requested_bitrate_kbps: self.requested_bitrate_kbps,
            input: &self.input,
            format: live.stream.as_ref().and_then(ClientStream::stream_format),
            stats: stats.as_ref(),
            rates: self.rates.as_ref(),
        }
        .readout();
        drop(live);

        let sections: Vec<AnyElement> = readout
            .sections
            .into_iter()
            .map(|section| {
                let action = (section.kind == SectionKind::Quality).then(|| {
                    button(
                        "quality-toggle",
                        if measuring {
                            "Stop measuring"
                        } else {
                            "Measure"
                        },
                        ButtonStyle::Quiet,
                        stats.is_some(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_quality(cx)))
                    .into_any_element()
                });
                render_section(section, action)
            })
            .collect();

        div()
            .id("connection-details")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(PANEL_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(theme::rule())
            .bg(theme::chrome())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .border_b_1()
                    .border_color(theme::rule())
                    .children(
                        readout
                            .figures
                            .into_iter()
                            .enumerate()
                            .map(|(position, figure)| render_figure(position, figure)),
                    ),
            )
            .child(
                div()
                    .id("connection-details-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .pb_4()
                    .children(sections),
            )
    }
}

fn render_figure(position: usize, figure: Figure) -> impl IntoElement {
    let known = figure.value.is_some();

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_0p5()
        .px_3()
        .py_3()
        .when(position > 0, |this| {
            this.border_l_1().border_color(theme::rule())
        })
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_1()
                .font_features(theme::tabular())
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if known {
                            theme::text()
                        } else {
                            theme::text_faint()
                        })
                        .child(figure.value.unwrap_or_else(|| NO_FIGURE.to_owned())),
                )
                .when(known, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_faint())
                            .child(figure.unit),
                    )
                }),
        )
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(theme::text_muted())
                .child(figure.label),
        )
}

fn render_section(section: Section, action: Option<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .pt_4()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .min_h(px(24.0))
                .pb_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(section.kind.title()),
                )
                .children(action),
        )
        .children(section.rows.into_iter().map(render_row))
        .into_any_element()
}

fn render_row(row: Row) -> impl IntoElement {
    let (value, color) = match row.value {
        Reading::Value(value) if row.attention => (value, theme::warning()),
        Reading::Value(value) => (value, theme::text()),
        Reading::Waiting => (WAITING.to_owned(), theme::text_faint()),
    };

    div()
        .flex()
        .items_baseline()
        .justify_between()
        .gap_3()
        .py(px(3.0))
        .text_xs()
        .child(
            div()
                .flex_none()
                .text_color(theme::text_muted())
                .child(row.label),
        )
        .child(
            div()
                .min_w_0()
                .text_right()
                .font_features(theme::tabular())
                .text_color(color)
                .child(value),
        )
}
