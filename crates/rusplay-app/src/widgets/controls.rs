use gpui::{Div, ElementId, SharedString, Stateful, div, prelude::*, px};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Quiet,
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    style: ButtonStyle,
    enabled: bool,
) -> Stateful<Div> {
    let (background, hover, foreground) = match style {
        ButtonStyle::Primary => (theme::accent(), theme::accent(), theme::accent_text()),
        ButtonStyle::Secondary => (
            theme::surface_raised(),
            theme::surface_hover(),
            theme::text(),
        ),
        ButtonStyle::Quiet => (
            theme::surface(),
            theme::surface_hover(),
            theme::text_muted(),
        ),
    };

    let base = div()
        .id(id)
        .h(px(30.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_sm()
        .bg(background)
        .text_color(foreground)
        .child(label.into());

    if enabled {
        base.cursor_pointer()
            .hover(move |style| style.bg(hover).opacity(0.9))
    } else {
        base.opacity(0.4)
    }
}

pub fn option_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let (background, foreground) = if selected {
        (theme::accent(), theme::accent_text())
    } else {
        (theme::surface_raised(), theme::text())
    };

    div()
        .id(id)
        .h(px(28.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .bg(background)
        .text_color(foreground)
        .when(!selected, |this| {
            this.hover(|style| style.bg(theme::surface_hover()))
        })
        .child(label.into())
}

pub fn toggle(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    let track = if on {
        theme::accent()
    } else {
        theme::surface_raised()
    };

    let knob = div().size(px(16.0)).rounded_full().bg(theme::accent_text());

    let mut container = div()
        .id(id)
        .w(px(40.0))
        .h(px(22.0))
        .px(px(3.0))
        .flex()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .bg(track)
        .border_1()
        .border_color(theme::border());

    if on {
        container = container.justify_end();
    }

    container.child(knob)
}

pub fn section_title(title: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .text_color(theme::text_muted())
        .pt_4()
        .pb_1()
        .child(title.into())
}
