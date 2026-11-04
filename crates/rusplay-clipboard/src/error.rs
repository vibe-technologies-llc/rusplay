use thiserror::Error;

#[cfg(feature = "portal")]
use crate::PortalStep;

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error(transparent)]
    Protocol(#[from] rusplay_protocol::ProtocolError),
    #[error("no wayland display to open the clipboard on")]
    NoWaylandDisplay,
    #[cfg(feature = "wayland")]
    #[error("connecting to the wayland display failed")]
    WaylandConnect(#[source] Box<wayland_client::ConnectError>),
    #[cfg(feature = "wayland")]
    #[error("connecting to the wayland socket failed")]
    WaylandSocket(#[source] std::io::Error),
    #[cfg(feature = "wayland")]
    #[error("listing the wayland globals failed")]
    WaylandGlobals(#[source] Box<wayland_client::globals::GlobalError>),
    #[cfg(feature = "wayland")]
    #[error("dispatching wayland events failed")]
    WaylandDispatch(#[source] Box<wayland_client::DispatchError>),
    #[cfg(feature = "wayland")]
    #[error("reading from or flushing to the wayland socket failed")]
    WaylandRead(#[source] Box<wayland_client::backend::WaylandError>),
    #[cfg(feature = "wayland")]
    #[error("waiting on the wayland socket failed")]
    WaylandPoll(#[source] std::io::Error),
    #[cfg(feature = "wayland")]
    #[error("starting the clipboard thread failed")]
    ThreadSpawn(#[source] std::io::Error),
    #[error("the compositor offers neither ext-data-control nor wlr-data-control")]
    DataControlMissing,
    #[error("the compositor offers no seat to read a clipboard from")]
    NoSeat,
    #[error("the clipboard backend stopped")]
    BackendStopped,
    #[error("the clipboard backends are not compiled in")]
    NotCompiled,
    #[cfg(feature = "portal")]
    #[error("the {step:?} portal call failed")]
    Portal {
        step: PortalStep,
        #[source]
        source: Box<ashpd::Error>,
    },
    #[cfg(feature = "portal")]
    #[error("the portal clipboard could not be set up")]
    PortalSetup(#[source] std::sync::Arc<ClipboardError>),
    #[cfg(feature = "portal")]
    #[error("starting the portal clipboard thread failed")]
    PortalSpawn(#[source] std::io::Error),
}
