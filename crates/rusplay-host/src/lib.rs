mod accept;
mod capture;
mod control;
mod encode;
mod error;
mod gpu;
mod mailbox;
mod outbox;
mod pipeline;
mod send;
mod session;
mod shared;
mod source;
mod stats;
#[cfg(test)]
mod tests;
mod upload;

pub use rusplay_capture::{DrmFourcc, Frame, FrameBuffer};
pub use rusplay_net::{NetError, PeerRecord, PeerStore, Pin};
pub use rusplay_protocol::PeerId;

pub use crate::{
    accept::{Admission, Client},
    error::{HostError, Stage},
    gpu::Gpu,
    session::{HostSession, HostStream},
    source::{CaptureFeed, FrameSource, open_portal_capture},
    stats::HostStats,
};
