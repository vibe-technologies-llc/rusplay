mod accept;
mod audio;
mod capture;
mod control;
mod cursor;
mod datagrams;
mod encode;
mod error;
mod gpu;
mod mailbox;
mod outbox;
mod pipeline;
mod route;
mod send;
mod session;
mod shared;
mod source;
mod stats;
#[cfg(test)]
mod tests;
mod upload;

pub use rusplay_capture::{DrmFourcc, Frame, FrameBuffer};
pub use rusplay_net::{DiscoveryResponder, NetError, PeerRecord, PeerStore, Pin};
pub use rusplay_protocol::PeerId;

pub use crate::{
    accept::{Admission, Client},
    audio::{AudioSource, AudioSourceFactory},
    error::{HostError, Stage},
    gpu::Gpu,
    session::{HostSession, HostStream},
    source::{CaptureFeed, FrameSource, open_portal_capture},
    stats::HostStats,
};
