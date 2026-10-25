use std::{env, path::PathBuf, process::ExitCode};

use rusplay_config::Config;
use rusplay_host::HostSession;

fn main() -> ExitCode {
    let config = match env::args_os().nth(1).map(PathBuf::from) {
        Some(path) => match Config::load(&path) {
            Ok(config) => config,
            Err(error) => return report(&error),
        },
        None => Config::default(),
    };

    match HostSession::new(config).run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report(&error),
    }
}

fn report(error: &dyn std::error::Error) -> ExitCode {
    eprintln!("rusplay-hostd: {error}");

    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }

    ExitCode::FAILURE
}
