mod announce;
mod audio;
mod bitrate;
mod clipboard;
mod clock;
mod color;
mod cursor;
mod decode;
mod display;
mod error;
mod formats;
#[cfg(test)]
mod hardware;
mod host_book;
mod input;
mod microphone;
mod name;
mod present;
mod receive;
mod resize;
mod session;
mod sink;
mod slot;
mod stats;
mod stats_json;
#[cfg(test)]
mod testing;
#[cfg(feature = "window")]
mod window;

pub use rusplay_audio::{AudioInput, AudioOutput, AudioOutputStats};
pub use rusplay_clipboard::{
    BackendKind, ClipboardBackend, ClipboardError, ClipboardPolicy, ClipboardState, FocusClipboard,
    FocusClipboardHandle, MemoryClipboard,
};
#[cfg(feature = "window")]
pub use rusplay_clipboard::{DataControl, DataControlOptions};
pub use rusplay_core::{StatsPhase, StatsSchedule, unix_millis_now};
#[cfg(feature = "window")]
pub use rusplay_input::capture::{ActivePointer, ClientAction};
pub use rusplay_net::{
    DiscoveredHost, DiscoveryBrowser, DiscoveryOptions, MAX_PEER_NAME_BYTES, PeerRecord,
    PeerSecret, Pin,
};
pub use rusplay_protocol::{ClientDisplay, CursorImage, HostId, StreamDescription};
pub use rusplay_scale::{ColorVolume, OutputLight};

#[cfg(feature = "window")]
pub use crate::window::{
    LogicalArea, OverlayText, PresentationFlags, PresentationStats, StreamWindow, WaylandTarget,
    WindowLayout, WindowSink,
};
pub use crate::{
    announce::announced_display,
    audio::default_audio_output,
    clipboard::ClipboardOpener,
    color::FrameColor,
    cursor::{CursorState, LatestCursor},
    display::{
        DisplayImage, LuminanceRange, Luminances, OutputDisplay, query_display, query_outputs,
        query_target, resolve_target,
    },
    error::{ClientError, Stage},
    formats::decoder_formats,
    host_book::{HostBook, SavedHost},
    input::InputHandle,
    microphone::default_microphone,
    session::{ClientSession, ClientStream, Credentials, Gpu},
    sink::{FrameSink, LatestFrame, PresentedFrame},
    stats::ClientStats,
    stats_json::stats_json_line,
};
