mod cli;
mod config;
mod control;
mod control_server;
mod discovery;
mod peers;
mod pin_file;
mod responder;
mod stats;
mod stats_json;

use std::{
    env,
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use rusplay_config::{Config, ConfigEnvironment, ConfigError};
use rusplay_host::{
    Admission, CaptureFeed, Gpu, HostError, HostSession, NetError, PeerStore, Pin,
    load_or_create_host_id, open_portal,
};
use thiserror::Error;

use crate::{
    cli::{CliError, Command, ConfigAction, HELP, PeersAction, VERSION},
    control::{ControlError, Request},
    control_server::{ControlContext, ControlServer, PinWindow},
    discovery::ConfigSource,
    peers::{PeersError, render_list, resolve},
    pin_file::{PinFile, PinFileError},
    responder::{Responder, ResponderError},
};

const USAGE_EXIT_CODE: u8 = 2;
const HOST_ID_FILE: &str = "host-id";
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
        Command::Run { config } => serve(load_config(config)?),
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
    host: &HostSession,
    address: SocketAddr,
    window: &Arc<PinWindow>,
) -> Option<ControlServer> {
    let config = host.config();
    if !config.peers.control_socket {
        return None;
    }

    let runtime_dir = pin_file::runtime_dir_from_process();
    let started = control::resolve(&config.peers, runtime_dir.as_deref()).and_then(|path| {
        ControlServer::start(
            &path,
            ControlContext {
                peers: Arc::clone(host.peers()),
                address,
                pin_digits: config.network.pairing_pin_digits,
                window: Arc::clone(window),
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

fn serve(config: Config) -> Result<(), HostdError> {
    let peers = PeerStore::open_host(&config.peers).map_err(HostError::from)?;
    warn_if_exposed(&peers);
    let host_id = load_or_create_host_id(&peers.path().with_file_name(HOST_ID_FILE))
        .map_err(HostError::from)?;
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
    let gpu = Gpu::open()?;
    let portal = open_portal(&config, &gpu)?;
    let feed = portal.feed;
    let mut host = HostSession::bind(config, peers)?;
    if let Some(input) = portal.input {
        host = host.with_input(input);
    }
    let address = host.local_addr()?;
    eprintln!("rusplay-hostd: listening on {address}");
    let window = Arc::new(PinWindow::new());
    let _control = start_control(&host, address, &window);
    let _responder = Responder::start(
        &host.config().network,
        address.port(),
        host.config().peers.host_name.as_deref(),
        host_id,
        host.busy_flag(),
    )?;

    loop {
        match serve_next(&host, &gpu, &feed, pin_file.as_ref(), &window) {
            Ok(()) => {}
            Err(error) if error.is_session_failure() => report(&error),
            Err(error) => return Err(error.into()),
        }
    }
}

fn serve_next(
    host: &HostSession,
    gpu: &Gpu,
    feed: &Arc<CaptureFeed>,
    pin_file: Option<&PinFile>,
    window: &PinWindow,
) -> Result<(), HostError> {
    let network = &host.config().network;
    let pin = window.open(
        Duration::from_millis(u64::from(network.pairing_timeout_ms)),
        || Pin::generate(network.pairing_pin_digits),
    )?;
    if let Some(file) = pin_file
        && let Err(error) = file.publish(pin.as_str())
    {
        report(&error);
    }
    eprintln!(
        "rusplay-hostd: pairing pin {} for the next {} s",
        pin.as_str(),
        network.pairing_timeout_ms / MILLIS_PER_SECOND
    );

    let accepted = host.accept_cancellable(Some(pin), window.cancel_flag());
    if let Some(file) = pin_file {
        file.clear();
    }
    let client = match accepted {
        Ok(client) => client,
        Err(HostError::AcceptTimeout(_) | HostError::AcceptCancelled) => return Ok(()),
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

    window.stream(address);
    let stream = host.serve(&client, gpu.clone(), feed)?;
    stats::wait_logging(stream, network.stats_log_interval_ms, network.stats_json)?;
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
