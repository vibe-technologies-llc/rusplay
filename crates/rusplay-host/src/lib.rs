mod accept;
mod audio;
mod capture;
mod clock;
mod control;
mod cursor;
mod datagrams;
mod encode;
mod error;
mod gpu;
mod handoff;
#[cfg(test)]
mod hardware;
mod input;
mod link;
mod mailbox;
mod microphone;
mod negotiate;
mod outbox;
mod pipeline;
mod portal;
mod priority;
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
pub use rusplay_core::{
    CodecFormat, FormatSet, JsonLine, StatsPhase, StatsSchedule, unix_millis_now,
};
pub use rusplay_net::{
    DiscoveryResponder, NetError, PeerRecord, PeerStore, Pin, load_or_create_host_id,
};
pub use rusplay_protocol::{HostId, PeerId};

pub use crate::{
    accept::{Admission, Client},
    audio::{AudioSource, AudioSourceFactory},
    error::{HostError, Stage},
    gpu::Gpu,
    input::{InputFeed, InputTarget},
    microphone::MicrophoneOutputFactory,
    negotiate::NegotiatedFormat,
    portal::{PortalHost, open_portal},
    session::{HostSession, HostStream},
    source::{CaptureFeed, FrameSource, open_portal_capture},
    stats::HostStats,
};
