use gpui::{
    AnyElement, Div, ElementId, FontWeight, Rgba, SharedString, Stateful, div, prelude::*, px,
};

use crate::theme;

const BUTTON_HEIGHT: f32 = 30.0;
const OPTION_HEIGHT: f32 = 26.0;
const TAB_HEIGHT: f32 = 39.0;
const DISABLED_OPACITY: f32 = 0.4;
const TOGGLE_WIDTH: f32 = 34.0;
const TOGGLE_HEIGHT: f32 = 20.0;
const TOGGLE_KNOB: f32 = 14.0;
const DOT_SIZE: f32 = 6.0;
const TAB_BAR_INSET: f32 = 10.0;

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
    Live,
    Warning,
    Danger,
    Neutral,
}

impl Tone {
    pub fn color(self) -> Rgba {
        match self {
            Self::Accent => theme::accent(),
            Self::Live => theme::live(),
            Self::Warning => theme::warning(),
            Self::Danger => theme::danger(),
            Self::Neutral => theme::text_muted(),
        }
    }
}

fn control(id: impl Into<ElementId>, height: f32) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(height))
        .flex()
        .flex_none()
        .items_center()
        .rounded(theme::radius_control())
        .text_sm()
        .whitespace_nowrap()
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    style: ButtonStyle,
    enabled: bool,
) -> Stateful<Div> {
    let base = control(id, BUTTON_HEIGHT)
        .px(px(12.0))
        .justify_center()
        .font_weight(FontWeight::MEDIUM)
        .child(label.into());

    let styled = match style {
        ButtonStyle::Primary if enabled => base
            .bg(theme::accent())
            .text_color(theme::accent_ink())
            .hover(|style| style.bg(theme::accent_hover())),
        ButtonStyle::Primary => base.bg(theme::raised()).text_color(theme::text_faint()),
        ButtonStyle::Secondary => base
            .bg(theme::raised())
            .border_1()
            .border_color(theme::rule_strong())
            .text_color(theme::text())
            .when(enabled, |this| {
                this.hover(|style| style.bg(theme::raised_hover()))
            }),
        ButtonStyle::Quiet => base.text_color(theme::text_muted()).when(enabled, |this| {
            this.hover(|style| style.bg(theme::raised()).text_color(theme::text()))
        }),
        ButtonStyle::Danger => base.text_color(theme::danger()).when(enabled, |this| {
            this.hover(|style| style.bg(theme::danger_quiet()))
        }),
    };

    match (enabled, style) {
        (true, _) => styled.cursor_pointer(),
        (false, ButtonStyle::Primary) => styled,
        (false, _) => styled.opacity(DISABLED_OPACITY),
    }
}

pub fn button_toggled(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on: bool,
) -> Stateful<Div> {
    let base = control(id, BUTTON_HEIGHT)
        .px(px(12.0))
        .justify_center()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label.into());

    if on {
        base.bg(theme::raised())
            .text_color(theme::text())
            .hover(|style| style.bg(theme::raised_hover()))
    } else {
        base.text_color(theme::text_muted())
            .hover(|style| style.bg(theme::raised()).text_color(theme::text()))
    }
}

pub fn option_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let base = control(id, OPTION_HEIGHT)
        .px_2p5()
        .border_1()
        .cursor_pointer()
        .child(label.into());

    if selected {
        base.bg(theme::accent_quiet())
            .border_color(theme::accent_quiet())
            .text_color(theme::text())
    } else {
        base.border_color(theme::rule_strong())
            .text_color(theme::text_muted())
            .hover(|style| style.bg(theme::raised()).text_color(theme::text()))
    }
}

pub fn tab_strip(tabs: impl IntoIterator<Item = Stateful<Div>>) -> Div {
    div().flex().items_center().gap_1().children(tabs)
}

pub fn tab(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .h(px(TAB_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(label.into())
        .map(|this| {
            if selected {
                this.text_color(theme::text()).child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left(px(TAB_BAR_INSET))
                        .right(px(TAB_BAR_INSET))
                        .h(px(theme::SELECTION_BAR))
                        .bg(theme::accent()),
                )
            } else {
                this.text_color(theme::text_muted())
                    .hover(|style| style.text_color(theme::text()))
            }
        })
}

pub fn segmented(segments: impl IntoIterator<Item = Stateful<Div>>) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_0p5()
        .p_0p5()
        .rounded(theme::radius_control())
        .bg(theme::inset())
        .border_1()
        .border_color(theme::rule())
        .children(segments)
}

pub fn segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let base = control(id, OPTION_HEIGHT)
        .px_3()
        .cursor_pointer()
        .child(label.into());

    if selected {
        base.bg(theme::raised())
            .text_color(theme::text())
            .font_weight(FontWeight::MEDIUM)
    } else {
        base.text_color(theme::text_muted())
            .hover(|style| style.text_color(theme::text()))
    }
}

pub fn menu_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .min_h(px(OPTION_HEIGHT))
        .pl_3()
        .pr_2()
        .flex()
        .items_center()
        .rounded(theme::radius_control())
        .text_sm()
        .cursor_pointer()
        .map(|this| {
            if selected {
                this.bg(theme::accent_quiet())
                    .text_color(theme::text())
                    .child(selection_bar())
            } else {
                this.text_color(theme::text_muted())
                    .hover(|style| style.bg(theme::raised_hover()).text_color(theme::text()))
            }
        })
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
}

pub fn select_trigger(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    open: bool,
) -> Stateful<Div> {
    control(id, BUTTON_HEIGHT)
        .px_2p5()
        .justify_between()
        .gap_3()
        .border_1()
        .border_color(if open {
            theme::accent()
        } else {
            theme::rule_strong()
        })
        .bg(theme::inset())
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|style| style.border_color(theme::text_faint()))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_faint())
                .child("\u{25be}"),
        )
}

pub fn toggle(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    let knob = div().size(px(TOGGLE_KNOB)).rounded_full().bg(if on {
        theme::accent_ink()
    } else {
        theme::text_faint()
    });

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
            .bg(theme::accent())
            .border_color(theme::accent())
            .hover(|style| style.bg(theme::accent_hover()))
    } else {
        container
            .bg(theme::inset())
            .border_color(theme::rule_strong())
            .hover(|style| style.border_color(theme::text_faint()))
    };

    container.child(knob)
}

pub fn selection_bar() -> Div {
    div()
        .absolute()
        .left_0()
        .top(px(6.0))
        .bottom(px(6.0))
        .w(px(theme::SELECTION_BAR))
        .rounded_full()
        .bg(theme::accent())
}

pub fn group_heading(title: impl Into<SharedString>) -> Div {
    div()
        .pt_6()
        .pb_2()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(title.into())
}

pub fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .p_4()
        .rounded(theme::radius_panel())
        .border_1()
        .border_color(theme::rule())
        .bg(theme::surface())
}

pub fn panel_heading(title: impl Into<SharedString>, action: Option<AnyElement>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .min_h(px(BUTTON_HEIGHT))
        .pb_3()
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme::text())
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .children(action)
}

pub fn state_text(label: impl Into<SharedString>, tone: Tone) -> Div {
    div()
        .flex_none()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(tone.color())
        .child(label.into())
}

pub fn status_dot(tone: Tone) -> Div {
    div()
        .flex_none()
        .size(px(DOT_SIZE))
        .rounded_full()
        .bg(tone.color())
}

pub fn value_row(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    color: Rgba,
) -> Div {
    div()
        .flex()
        .items_baseline()
        .justify_between()
        .gap_4()
        .py_1()
        .text_sm()
        .child(
            div()
                .flex_none()
                .text_color(theme::text_muted())
                .child(label.into()),
        )
        .child(
            div()
                .min_w_0()
                .text_right()
                .font_features(theme::tabular())
                .text_color(color)
                .child(value.into()),
        )
}
