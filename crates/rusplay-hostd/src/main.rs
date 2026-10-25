mod cli;
mod discovery;

use std::{env, error::Error, path::Path, process::ExitCode};

use rusplay_config::{Config, ConfigError};
use rusplay_host::{HostError, HostSession};
use thiserror::Error;

use crate::{
    cli::{CliError, Command, HELP, VERSION},
    discovery::{ConfigSource, Environment},
};

const USAGE_EXIT_CODE: u8 = 2;

#[derive(Debug, Error)]
enum HostdError {
    #[error(transparent)]
    Cli(#[from] CliError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Host(#[from] HostError),
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error @ HostdError::Cli(_)) => {
            report(&error);
            eprintln!("try rusplay-hostd --help");
            ExitCode::from(USAGE_EXIT_CODE)
        }
        Err(error) => {
            report(&error);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), HostdError> {
    match cli::parse(env::args_os().skip(1))? {
        Command::Help => {
            print!("{HELP}");
            Ok(())
        }
        Command::Version => {
            println!("{VERSION}");
            Ok(())
        }
        Command::Run { config } => {
            let source = discovery::resolve(config, &Environment::from_process(), Path::is_file);
            let config = load(&source)?;

            HostSession::new(config).run()?;
            Ok(())
        }
    }
}

fn load(source: &ConfigSource) -> Result<Config, ConfigError> {
    match source {
        ConfigSource::Explicit(path) | ConfigSource::Discovered(path) => {
            eprintln!("rusplay-hostd: reading {}", path.display());
            Config::load(path)
        }
        ConfigSource::Defaults => {
            eprintln!("rusplay-hostd: no config file found, using the defaults");
            Ok(Config::default())
        }
    }
}

fn report(error: &dyn Error) {
    eprintln!("rusplay-hostd: {error}");

    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
}
