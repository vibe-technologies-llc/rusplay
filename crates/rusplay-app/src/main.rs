mod app;
mod hosts;
mod notice;
mod session;
mod settings;
mod stats;
mod stream;
mod theme;
mod widgets;

use gpui::{App, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_platform::application;
use rusplay_config::ConfigEnvironment;

use crate::app::RusplayApp;

const WINDOW_WIDTH: f32 = 1100.0;
const WINDOW_HEIGHT: f32 = 760.0;
const WINDOW_TITLE: &str = "Rusplay";

fn main() {
    application().run(|cx: &mut App| {
        app::bind_keys(cx);
        widgets::text_field::bind_keys(cx);

        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(WINDOW_TITLE.into()),
                ..gpui::TitlebarOptions::default()
            }),
            app_id: Some("rusplay".to_owned()),
            ..WindowOptions::default()
        };

        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| RusplayApp::new(ConfigEnvironment::from_process(), window, cx));
            window.focus(&gpui::Focusable::focus_handle(&view, cx), cx);
            view
        });

        match opened {
            Ok(_) => cx.activate(true),
            Err(error) => {
                eprintln!("rusplay: could not open the window: {error:#}");
                cx.quit();
            }
        }
    });
}
