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
