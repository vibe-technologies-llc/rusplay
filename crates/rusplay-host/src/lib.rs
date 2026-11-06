mod accept;
mod audio;
mod capture;
mod clipboard;
mod clock;
mod control;
mod cursor;
mod datagrams;
mod encode;
mod error;
mod formats;
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
mod responder;
mod route;
mod send;
mod service;
mod session;
mod shared;
mod source;
mod stats;
#[cfg(test)]
mod tests;
mod upload;
mod virtual_output;

pub use rusplay_capture::{DrmFourcc, Frame, FrameBuffer};
pub use rusplay_clipboard::{BackendKind, ClipboardError, ClipboardState};
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
    clipboard::ClipboardOpener,
    error::{HostError, Stage},
    formats::encoder_formats,
    gpu::Gpu,
    input::{InputFeed, InputTarget},
    microphone::MicrophoneOutputFactory,
    negotiate::NegotiatedFormat,
    portal::{PortalHost, open_portal},
    responder::Responder,
    service::{HostEvent, HostService, PinRenewer, StreamSnapshot},
    session::{HostSession, HostStream},
    source::{CaptureFeed, FrameSource, open_portal_capture},
    stats::HostStats,
};
