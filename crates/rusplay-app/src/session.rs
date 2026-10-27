use std::{net::SocketAddr, time::Duration};

use gpui::Window;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use rusplay_client::{
    ClientError, ClientSession, ClientStream, Credentials, DiscoveredHost, DiscoveryBrowser,
    DiscoveryOptions, Gpu, StreamWindow, WaylandTarget, WindowSink,
};
use rusplay_config::{Config, ConfigEnvironment, ConfigError, ConfigRole};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("the window is not a wayland surface")]
    NotWayland,
}

pub fn load_client_config(environment: &ConfigEnvironment) -> Result<Config, ConfigError> {
    let path = environment.config_path(ConfigRole::Client)?;

    Config::load_or_default(&path)
}

pub fn discover(environment: &ConfigEnvironment) -> Result<Vec<DiscoveredHost>, SessionError> {
    let config = load_client_config(environment)?;
    let scan_time = Duration::from_millis(u64::from(config.network.discovery_scan_ms));

    let browser = DiscoveryBrowser::with_options(DiscoveryOptions::from(&config.network))
        .map_err(ClientError::from)?;

    Ok(browser.scan(scan_time).map_err(ClientError::from)?)
}

pub fn attach_window(window: &Window, config: &Config) -> Result<StreamWindow, SessionError> {
    let target = wayland_target(window).ok_or(SessionError::NotWayland)?;

    Ok(unsafe { StreamWindow::attach(target, config) }?)
}

pub fn connect(
    config: Config,
    host: SocketAddr,
    credentials: &Credentials,
    gpu: Gpu,
    sink: WindowSink,
) -> Result<ClientStream, SessionError> {
    Ok(ClientSession::new(config).connect(host, credentials, gpu, sink)?)
}

fn wayland_target(window: &Window) -> Option<WaylandTarget> {
    let RawDisplayHandle::Wayland(display) =
        HasDisplayHandle::display_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    let RawWindowHandle::Wayland(surface) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };

    Some(WaylandTarget {
        display: display.display,
        surface: surface.surface,
    })
}
