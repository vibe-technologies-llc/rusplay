use gpui::{
    AnyElement, App, Bounds, ClickEvent, CursorStyle, Decorations, Div, HitboxBehavior,
    MouseButton, Pixels, Point, ResizeEdge, Size, Stateful, Tiling, Window, canvas, div, point,
    prelude::*, px,
};

use crate::theme;

pub const RESIZE_INSET: f32 = 10.0;
const CORNER_RADIUS: f32 = 8.0;
const CONTROL_WIDTH: f32 = 42.0;
const CONTROL_HEIGHT: f32 = 30.0;
const GLYPH_SIZE: f32 = 10.0;
const GLYPH_STROKE: f32 = 1.5;
const RESTORE_OFFSET: f32 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    Server,
    Floating,
    Tiled(Tiling),
}

impl Frame {
    pub fn of(window: &Window) -> Self {
        match window.window_decorations() {
            Decorations::Server => Self::Server,
            Decorations::Client { tiling } if tiling.is_tiled() => Self::Tiled(tiling),
            Decorations::Client { .. } => Self::Floating,
        }
    }

    pub const fn is_client(self) -> bool {
        !matches!(self, Self::Server)
    }

    pub const fn rounded(self) -> bool {
        matches!(self, Self::Floating)
    }

    const fn tiling(self) -> Tiling {
        match self {
            Self::Server | Self::Floating => Tiling {
                top: false,
                left: false,
                right: false,
                bottom: false,
            },
            Self::Tiled(tiling) => tiling,
        }
    }
}

pub fn corner_radius() -> Pixels {
    px(CORNER_RADIUS)
}

pub fn frame(window: &mut Window, body: Stateful<Div>) -> AnyElement {
    let frame = Frame::of(window);
    if !frame.is_client() {
        return body.size_full().into_any_element();
    }

    window.set_client_inset(px(RESIZE_INSET));
    let tiling = frame.tiling();
    let inset = px(RESIZE_INSET);
    let radius = corner_radius();

    let body = body
        .size_full()
        .cursor(CursorStyle::Arrow)
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
        .when(frame.rounded(), |this| {
            this.rounded(radius)
                .border_1()
                .border_color(theme::rule_strong())
                .shadow(theme::window_shadow())
        });

    div()
        .id("window-frame")
        .size_full()
        .child(resize_cursor_layer())
        .when(!tiling.top, |this| this.pt(inset))
        .when(!tiling.bottom, |this| this.pb(inset))
        .when(!tiling.left, |this| this.pl(inset))
        .when(!tiling.right, |this| this.pr(inset))
        .on_mouse_move(|_, window, _| window.refresh())
        .on_mouse_down(MouseButton::Left, move |event, window, _| {
            let size = window.window_bounds().get_bounds().size;
            if let Some(edge) = resize_edge(event.position, inset, size, tiling) {
                window.start_window_resize(edge);
            }
        })
        .child(body)
        .into_any_element()
}

fn resize_cursor_layer() -> impl IntoElement {
    canvas(
        |_, window, _| {
            window.insert_hitbox(
                Bounds::new(
                    point(px(0.0), px(0.0)),
                    window.window_bounds().get_bounds().size,
                ),
                HitboxBehavior::Normal,
            )
        },
        |_, hitbox, window, _| {
            let size = window.window_bounds().get_bounds().size;
            let tiling = Frame::of(window).tiling();
            let Some(edge) = resize_edge(window.mouse_position(), px(RESIZE_INSET), size, tiling)
            else {
                return;
            };
            window.set_cursor_style(resize_cursor(edge), &hitbox);
        },
    )
    .size_full()
    .absolute()
}

const fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

pub fn resize_edge(
    position: Point<Pixels>,
    inset: Pixels,
    size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let top = !tiling.top && position.y < inset;
    let bottom = !tiling.bottom && position.y > size.height - inset;
    let left = !tiling.left && position.x < inset;
    let right = !tiling.right && position.x > size.width - inset;

    match (top, bottom, left, right) {
        (true, _, true, _) => Some(ResizeEdge::TopLeft),
        (true, _, _, true) => Some(ResizeEdge::TopRight),
        (_, true, true, _) => Some(ResizeEdge::BottomLeft),
        (_, true, _, true) => Some(ResizeEdge::BottomRight),
        (true, ..) => Some(ResizeEdge::Top),
        (_, true, ..) => Some(ResizeEdge::Bottom),
        (_, _, true, _) => Some(ResizeEdge::Left),
        (.., true) => Some(ResizeEdge::Right),
        _ => None,
    }
}

pub fn window_controls(
    window: &Window,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let supported = window.window_controls();
    let maximized = window.is_maximized();

    div()
        .flex()
        .flex_none()
        .items_center()
        .occlude()
        .when(supported.minimize, |this| {
            this.child(
                control("window-minimize", false)
                    .on_click(|_, window, _| window.minimize_window())
                    .child(minimize_glyph()),
            )
        })
        .when(supported.maximize, |this| {
            this.child(
                control("window-maximize", false)
                    .on_click(|_, window, _| window.zoom_window())
                    .child(if maximized {
                        restore_glyph().into_any_element()
                    } else {
                        maximize_glyph().into_any_element()
                    }),
            )
        })
        .child(
            control("window-close", true)
                .on_click(on_close)
                .child(close_glyph()),
        )
}

fn control(id: &'static str, closes: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(CONTROL_WIDTH))
        .h(px(CONTROL_HEIGHT))
        .rounded(theme::radius_control())
        .cursor_pointer()
        .text_color(theme::text_muted())
        .hover(move |style| {
            if closes {
                style.bg(theme::danger()).text_color(theme::text())
            } else {
                style.bg(theme::raised()).text_color(theme::text())
            }
        })
}

fn minimize_glyph() -> impl IntoElement {
    div()
        .w(px(GLYPH_SIZE))
        .h(px(GLYPH_STROKE))
        .rounded_full()
        .bg(theme::text_muted())
}

fn maximize_glyph() -> impl IntoElement {
    div()
        .size(px(GLYPH_SIZE))
        .rounded_sm()
        .border(px(GLYPH_STROKE))
        .border_color(theme::text_muted())
}

fn restore_glyph() -> impl IntoElement {
    let box_size = px(GLYPH_SIZE - RESTORE_OFFSET);

    div()
        .relative()
        .size(px(GLYPH_SIZE))
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .size(box_size)
                .rounded_sm()
                .border(px(GLYPH_STROKE))
                .border_color(theme::text_faint()),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .size(box_size)
                .rounded_sm()
                .border(px(GLYPH_STROKE))
                .border_color(theme::text_muted())
                .bg(theme::chrome()),
        )
}

fn close_glyph() -> impl IntoElement {
    div()
        .text_lg()
        .line_height(px(GLYPH_SIZE * 2.0))
        .child("\u{00d7}")
}

#[cfg(test)]
mod tests {
    use gpui::size;

    use super::*;

    const UNTILED: Tiling = Tiling {
        top: false,
        left: false,
        right: false,
        bottom: false,
    };

    fn edge_at(x: f32, y: f32, tiling: Tiling) -> Option<ResizeEdge> {
        resize_edge(
            point(px(x), px(y)),
            px(RESIZE_INSET),
            size(px(800.0), px(600.0)),
            tiling,
        )
    }

    #[test]
    fn corners_win_over_the_edges_they_join() {
        assert_eq!(edge_at(2.0, 2.0, UNTILED), Some(ResizeEdge::TopLeft));
        assert_eq!(edge_at(798.0, 2.0, UNTILED), Some(ResizeEdge::TopRight));
        assert_eq!(edge_at(2.0, 598.0, UNTILED), Some(ResizeEdge::BottomLeft));
        assert_eq!(
            edge_at(798.0, 598.0, UNTILED),
            Some(ResizeEdge::BottomRight)
        );
    }

    #[test]
    fn the_inside_of_the_window_is_not_an_edge() {
        assert_eq!(edge_at(400.0, 2.0, UNTILED), Some(ResizeEdge::Top));
        assert_eq!(edge_at(2.0, 300.0, UNTILED), Some(ResizeEdge::Left));
        assert_eq!(edge_at(400.0, 300.0, UNTILED), None);
    }

    #[test]
    fn a_tiled_side_offers_no_resize_edge() {
        let tiled_left = Tiling {
            left: true,
            ..UNTILED
        };

        assert_eq!(edge_at(2.0, 300.0, tiled_left), None);
        assert_eq!(edge_at(2.0, 2.0, tiled_left), Some(ResizeEdge::Top));
    }
}
