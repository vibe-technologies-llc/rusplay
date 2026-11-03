mod app;
mod chrome;
mod cli;
mod host_page;
mod hosting;
mod hosts;
mod notice;
mod session;
mod settings;
mod stats;
mod stream;
mod theme;
mod widgets;

use std::{env, process::ExitCode};

use gpui::{App, Bounds, WindowBounds, WindowDecorations, WindowOptions, prelude::*, px, size};
use gpui_platform::application;
use rusplay_config::ConfigEnvironment;

use crate::{
    app::RusplayApp,
    cli::{CliError, Command, HELP, Launch, VERSION},
};

const WINDOW_WIDTH: f32 = 1100.0;
const WINDOW_HEIGHT: f32 = 760.0;
const WINDOW_TITLE: &str = "Rusplay";

const USAGE_EXIT_CODE: u8 = 2;

fn main() -> ExitCode {
    match cli::parse(env::args_os().skip(1)) {
        Ok(Command::Run(launch)) => {
            run(launch);
            ExitCode::SUCCESS
        }
        Ok(Command::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("{VERSION}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            report(&error);
            ExitCode::from(USAGE_EXIT_CODE)
        }
    }
}

fn report(error: &CliError) {
    eprintln!("rusplay: {error}");
    eprintln!("try rusplay --help");
}

fn run(launch: Launch) {
    application().run(move |cx: &mut App| {
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
            window_decorations: Some(WindowDecorations::Client),
            ..WindowOptions::default()
        };

        let opened = cx.open_window(options, |window, cx| {
            let view =
                cx.new(|cx| RusplayApp::new(ConfigEnvironment::from_process(), launch, window, cx));
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
