mod audio;
mod color;
mod cursor;
mod decode;
mod display;
mod error;
mod host_book;
mod input;
mod present;
mod receive;
mod session;
mod sink;
mod slot;
mod stats;
#[cfg(test)]
mod testing;
#[cfg(feature = "window")]
mod window;

pub use rusplay_net::{
    DiscoveredHost, DiscoveryBrowser, DiscoveryOptions, MAX_PEER_NAME_BYTES, PeerRecord,
    PeerSecret, Pin,
};
pub use rusplay_protocol::{CursorImage, StreamDescription};
pub use rusplay_scale::{ColorVolume, OutputLight};

#[cfg(feature = "window")]
pub use crate::window::{LogicalArea, StreamWindow, WaylandTarget, WindowLayout, WindowSink};
pub use crate::{
    audio::{AudioOutput, AudioOutputStats, default_audio_output},
    color::FrameColor,
    cursor::{CursorState, LatestCursor},
    display::{
        DisplayImage, LuminanceRange, Luminances, OutputDisplay, query_display, query_outputs,
        query_target, resolve_target,
    },
    error::{ClientError, Stage},
    host_book::{HostBook, SavedHost},
    input::InputHandle,
    session::{ClientSession, ClientStream, Credentials, Gpu},
    sink::{FrameSink, LatestFrame, PresentedFrame},
    stats::ClientStats,
};
