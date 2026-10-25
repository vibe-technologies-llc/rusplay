mod cli;
mod discovery;
mod responder;

use std::{env, error::Error, path::Path, process::ExitCode, sync::Arc};

use rusplay_config::{Config, ConfigError};
use rusplay_host::{
    Admission, CaptureFeed, Gpu, HostError, HostSession, NetError, PeerStore, Pin,
    open_portal_capture,
};
use thiserror::Error;

use crate::{
    cli::{CliError, Command, HELP, VERSION},
    discovery::{ConfigSource, Environment},
    responder::{Responder, ResponderError},
};

const USAGE_EXIT_CODE: u8 = 2;
const MILLIS_PER_SECOND: u32 = 1_000;

#[derive(Debug, Error)]
enum HostdError {
    #[error(transparent)]
    Cli(#[from] CliError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Discovery(#[from] ResponderError),
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

            serve(config)
        }
    }
}

fn serve(config: Config) -> Result<(), HostdError> {
    let peers = PeerStore::open_host(&config.peers).map_err(HostError::from)?;
    let feed = open_portal_capture(&config)?;
    let gpu = Gpu::open()?;
    let host = HostSession::bind(config, peers)?;
    let address = host.local_addr()?;
    eprintln!("rusplay-hostd: listening on {address}");
    let _responder = Responder::start(
        &host.config().network,
        address.port(),
        host.config().peers.host_name.as_deref(),
    )?;

    loop {
        match serve_next(&host, &gpu, &feed) {
            Ok(()) => {}
            Err(error) if error.is_session_failure() => report(&error),
            Err(error) => return Err(error.into()),
        }
    }
}

fn serve_next(host: &HostSession, gpu: &Gpu, feed: &Arc<CaptureFeed>) -> Result<(), HostError> {
    let network = &host.config().network;
    let pin = Pin::generate(network.pairing_pin_digits)?;
    eprintln!(
        "rusplay-hostd: pairing pin {} for the next {} s",
        pin.as_str(),
        network.pairing_timeout_ms / MILLIS_PER_SECOND
    );

    let client = match host.accept(Some(pin)) {
        Ok(client) => client,
        Err(HostError::AcceptTimeout(_)) => return Ok(()),
        Err(HostError::Net(NetError::PairingLocked)) => {
            eprintln!("rusplay-hostd: too many wrong pins, issuing a new one");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let address = client.address();
    match client.admission() {
        Admission::Paired => eprintln!("rusplay-hostd: paired and streaming to {address}"),
        Admission::Reconnected => eprintln!("rusplay-hostd: streaming to {address}"),
    }

    host.serve(&client, gpu.clone(), feed)?.wait()?;
    eprintln!("rusplay-hostd: {address} left");
    Ok(())
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
