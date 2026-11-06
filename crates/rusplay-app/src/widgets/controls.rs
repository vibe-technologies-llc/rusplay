use gpui::{
    AnyElement, Div, ElementId, FontWeight, Rgba, SharedString, Stateful, div, prelude::*, px,
};

use crate::theme;

const BUTTON_HEIGHT: f32 = 32.0;
const OPTION_HEIGHT: f32 = 28.0;
const DISABLED_OPACITY: f32 = 0.4;
const TOGGLE_WIDTH: f32 = 38.0;
const TOGGLE_HEIGHT: f32 = 22.0;
const TOGGLE_KNOB: f32 = 16.0;
const DOT_SIZE: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Quiet,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Success,
    Warning,
    Danger,
    Neutral,
}

impl Tone {
    fn colors(self) -> (Rgba, Rgba) {
        match self {
            Self::Accent => (theme::accent_bright(), theme::accent_soft()),
            Self::Success => (theme::success(), theme::success_soft()),
            Self::Warning => (theme::warning(), theme::warning_soft()),
            Self::Danger => (theme::danger(), theme::danger_soft()),
            Self::Neutral => (theme::text_muted(), theme::surface_raised()),
        }
    }
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    style: ButtonStyle,
    enabled: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(BUTTON_HEIGHT))
        .px(px(14.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_lg()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .child(label.into());

    let styled = match style {
        ButtonStyle::Primary => base
            .bg(theme::brand())
            .text_color(theme::accent_text())
            .when(enabled, |this| {
                this.shadow(theme::glow())
                    .hover(|style| style.bg(theme::brand_hover()))
            }),
        ButtonStyle::Secondary => base
            .bg(theme::surface_raised())
            .border_1()
            .border_color(theme::border_strong())
            .text_color(theme::text())
            .when(enabled, |this| {
                this.hover(|style| style.bg(theme::surface_hover()))
            }),
        ButtonStyle::Quiet => base.text_color(theme::text_muted()).when(enabled, |this| {
            this.hover(|style| style.bg(theme::surface_hover()).text_color(theme::text()))
        }),
        ButtonStyle::Danger => base.text_color(theme::danger()).when(enabled, |this| {
            this.hover(|style| style.bg(theme::danger_soft()))
        }),
    };

    if enabled {
        styled.cursor_pointer()
    } else {
        styled.opacity(DISABLED_OPACITY)
    }
}

pub fn option_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(OPTION_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .text_sm()
        .cursor_pointer()
        .child(label.into());

    if selected {
        base.bg(theme::accent_soft())
            .border_color(theme::accent())
            .text_color(theme::accent_bright())
            .font_weight(FontWeight::MEDIUM)
    } else {
        base.bg(theme::surface_raised())
            .border_color(theme::border())
            .text_color(theme::text_muted())
            .hover(|style| {
                style
                    .bg(theme::surface_hover())
                    .border_color(theme::border_strong())
                    .text_color(theme::text())
            })
    }
}

pub fn tab_strip(tabs: impl IntoIterator<Item = Stateful<Div>>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .p_1()
        .rounded_lg()
        .bg(theme::inset())
        .border_1()
        .border_color(theme::border())
        .children(tabs)
}

pub fn tab(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(OPTION_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .rounded_md()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label.into());

    if selected {
        base.bg(theme::surface_hover())
            .text_color(theme::text())
            .shadow(theme::lift())
    } else {
        base.text_color(theme::text_muted())
            .hover(|style| style.text_color(theme::text()))
    }
}

pub fn segmented(segments: impl IntoIterator<Item = Stateful<Div>>) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_0p5()
        .p_0p5()
        .rounded_lg()
        .bg(theme::inset())
        .border_1()
        .border_color(theme::border())
        .children(segments)
}

pub fn segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(OPTION_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .rounded_md()
        .text_sm()
        .whitespace_nowrap()
        .cursor_pointer()
        .child(label.into());

    if selected {
        base.bg(theme::surface_hover())
            .text_color(theme::text())
            .font_weight(FontWeight::MEDIUM)
    } else {
        base.text_color(theme::text_muted())
            .hover(|style| style.bg(theme::surface()).text_color(theme::text()))
    }
}

pub fn menu_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .min_h(px(OPTION_HEIGHT))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .when(selected, |this| this.bg(theme::accent_soft()))
        .when(!selected, |this| {
            this.hover(|style| style.bg(theme::surface_hover()))
        })
        .child(
            div()
                .w(px(14.0))
                .flex_none()
                .text_xs()
                .text_color(theme::accent_bright())
                .child(if selected { "\u{2713}" } else { "" }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(if selected {
                    theme::accent_bright()
                } else {
                    theme::text()
                })
                .when(selected, |this| this.font_weight(FontWeight::MEDIUM))
                .child(label.into()),
        )
}

pub fn select_trigger(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    open: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(BUTTON_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .rounded_lg()
        .border_1()
        .border_color(if open {
            theme::accent()
        } else {
            theme::border_strong()
        })
        .bg(theme::inset())
        .text_sm()
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|style| style.border_color(theme::accent()))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(if open { "\u{25b4}" } else { "\u{25be}" }),
        )
}

pub fn toggle(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    let knob = div()
        .size(px(TOGGLE_KNOB))
        .rounded_full()
        .bg(if on {
            theme::accent_text()
        } else {
            theme::text_muted()
        })
        .shadow(theme::lift());

    let container = div()
        .id(id)
        .w(px(TOGGLE_WIDTH))
        .h(px(TOGGLE_HEIGHT))
        .px(px(2.0))
        .flex()
        .flex_none()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .border_1();

    let container = if on {
        container
            .justify_end()
            .bg(theme::brand())
            .border_color(theme::accent())
    } else {
        container
            .bg(theme::inset())
            .border_color(theme::border_strong())
            .hover(|style| style.border_color(theme::text_subtle()))
    };

    container.child(knob)
}

pub fn section_title(title: impl Into<SharedString>) -> Div {
    let title: SharedString = title.into();

    div()
        .flex()
        .items_center()
        .gap_3()
        .pt_5()
        .pb_2()
        .child(
            div()
                .flex_none()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_subtle())
                .child(title.to_uppercase()),
        )
        .child(div().flex_1().h(px(1.0)).bg(theme::border()))
}

pub fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .p_4()
        .rounded_xl()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .shadow(theme::lift())
}

pub fn card_heading(title: impl Into<SharedString>, detail: Option<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .pb_3()
        .child(
            div()
                .text_color(theme::text())
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .children(detail.map(|detail| {
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(detail)
        }))
}

pub fn badge(label: impl Into<SharedString>, tone: Tone) -> Div {
    let (foreground, background) = tone.colors();

    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(20.0))
        .px_2()
        .rounded_full()
        .bg(background)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(foreground)
        .child(label.into())
}

pub fn status_dot(tone: Tone) -> Div {
    let (color, halo) = tone.colors();

    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(DOT_SIZE * 2.0))
        .rounded_full()
        .bg(halo)
        .child(div().size(px(DOT_SIZE)).rounded_full().bg(color))
}

pub fn brand_mark(size: f32) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_lg()
        .bg(theme::brand())
        .shadow(theme::glow())
        .text_color(theme::accent_text())
        .font_weight(FontWeight::BOLD)
        .child("R")
        .into_any_element()
}
