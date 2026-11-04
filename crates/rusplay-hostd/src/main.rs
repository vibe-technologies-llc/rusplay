mod cli;
mod config;
mod control;
mod control_server;
mod discovery;
mod peers;
mod pin_file;
mod stats;
mod stats_json;
mod supervisor;

use std::{
    env,
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use parking_lot::RwLock;
use rusplay_config::{Config, ConfigEnvironment, ConfigError};
use rusplay_host::{HostError, HostService, PeerStore};
use thiserror::Error;

use crate::{
    cli::{CliError, Command, ConfigAction, HELP, PeersAction, VERSION},
    control::{ControlError, Request},
    control_server::{ControlContext, ControlServer, PinRequest, SharedPhase},
    discovery::ConfigSource,
    peers::{PeersError, render_list, resolve},
    pin_file::{PinFile, PinFileError},
    supervisor::Supervisor,
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
    #[error(transparent)]
    Peers(#[from] PeersError),
    #[error(transparent)]
    PinFile(#[from] PinFileError),
    #[error(transparent)]
    Control(#[from] ControlError),
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
        Command::Run { config } => serve(&load_config(config)?),
        Command::Peers { config, action } => manage_peers(&load_config(config)?, &action),
        Command::Config { config, action } => manage_config(config, action),
        Command::Pin { config } => show_pin(&load_config(config)?),
        Command::Control { config, request } => send_request(&load_config(config)?, &request),
    }
}

fn manage_config(path: Option<PathBuf>, action: ConfigAction) -> Result<(), HostdError> {
    match action {
        ConfigAction::Default => print!("{}", config::default_toml()?),
        ConfigAction::Check => {
            let source =
                discovery::resolve(path, &ConfigEnvironment::from_process(), Path::is_file);
            println!("{}", config::check(&source)?);
        }
    }

    Ok(())
}

fn show_pin(config: &Config) -> Result<(), HostdError> {
    let runtime_dir = pin_file::runtime_dir_from_process();
    let path = pin_file::resolve(&config.peers, runtime_dir.as_deref())?;
    println!("{}", pin_file::read(&path)?);

    Ok(())
}

fn send_request(config: &Config, request: &Request) -> Result<(), HostdError> {
    let runtime_dir = pin_file::runtime_dir_from_process();
    let path = control::resolve(&config.peers, runtime_dir.as_deref())?;
    print!("{}", control::send(&path, request)?);

    Ok(())
}

fn start_control(
    config: &Config,
    peers: &Arc<RwLock<PeerStore>>,
    address: SocketAddr,
    phase: &SharedPhase,
    request_pin: PinRequest,
) -> Option<ControlServer> {
    if !config.peers.control_socket {
        return None;
    }

    let runtime_dir = pin_file::runtime_dir_from_process();
    let started = control::resolve(&config.peers, runtime_dir.as_deref()).and_then(|path| {
        ControlServer::start(
            &path,
            ControlContext {
                peers: Arc::clone(peers),
                address,
                pin_digits: config.network.pairing_pin_digits,
                phase: phase.clone(),
                request_pin,
            },
        )
    });

    match started {
        Ok(server) => {
            eprintln!("rusplay-hostd: control socket {}", server.path().display());
            Some(server)
        }
        Err(error) => {
            eprintln!("rusplay-hostd: running without a control socket");
            report(&error);
            None
        }
    }
}

fn load_config(explicit: Option<PathBuf>) -> Result<Config, ConfigError> {
    let source = discovery::resolve(explicit, &ConfigEnvironment::from_process(), Path::is_file);

    load(&source)
}

fn manage_peers(config: &Config, action: &PeersAction) -> Result<(), HostdError> {
    let mut store = PeerStore::open_host(&config.peers).map_err(HostError::from)?;
    warn_if_exposed(&store);

    match action {
        PeersAction::List => print!("{}", render_list(&store)),
        PeersAction::Forget { target } => {
            let id = resolve(&store, target)?;
            let name = store.get(&id).map(|record| record.name().to_owned());
            store.forget(&id).map_err(HostError::from)?;
            println!("forgot {id} {}", name.unwrap_or_default());
        }
    }

    Ok(())
}

fn warn_if_exposed(store: &PeerStore) {
    if let Some(exposure) = store.exposure() {
        eprintln!(
            "rusplay-hostd: warning: the peer store {} {exposure}",
            store.path().display()
        );
    }
}

fn serve(config: &Config) -> Result<(), HostdError> {
    let store = PeerStore::open_host(&config.peers).map_err(HostError::from)?;
    warn_if_exposed(&store);
    let pin_file = PinFile::open(
        &config.peers,
        pin_file::runtime_dir_from_process().as_deref(),
    )?;
    if let Some(file) = &pin_file {
        eprintln!(
            "rusplay-hostd: writing the pairing pin to {}",
            file.path().display()
        );
    }

    let peers = Arc::new(RwLock::new(store));
    let service = HostService::start(config.clone(), &peers)?;
    let address = service.address();
    eprintln!("rusplay-hostd: listening on {address}");

    let phase = SharedPhase::new();
    let renewer = service.pin_renewer();
    let _control = start_control(
        config,
        &peers,
        address,
        &phase,
        Arc::new(move |pin| renewer.renew(pin)),
    );

    Supervisor::new(pin_file, phase, &config.network).run(&service)?;

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
