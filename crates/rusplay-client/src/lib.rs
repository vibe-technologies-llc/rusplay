mod audio;
mod color;
mod cursor;
mod decode;
mod display;
mod error;
mod present;
mod receive;
mod session;
mod sink;
mod slot;
mod stats;

pub use rusplay_net::{PeerRecord, PeerSecret, Pin};
pub use rusplay_protocol::CursorImage;
pub use rusplay_scale::OutputLight;

pub use crate::{
    audio::{AudioOutput, AudioOutputStats, default_audio_output},
    color::FrameColor,
    cursor::{CursorState, LatestCursor},
    display::{
        DisplayImage, LuminanceRange, Luminances, OutputDisplay, query_display, query_outputs,
        query_target, resolve_target,
    },
    error::{ClientError, Stage},
    session::{ClientSession, ClientStream, Credentials, Gpu},
    sink::{FrameSink, LatestFrame, PresentedFrame},
    stats::ClientStats,
};
