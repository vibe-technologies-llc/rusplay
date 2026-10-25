mod color;
mod decode;
mod error;
mod present;
mod receive;
mod session;
mod sink;
mod slot;
mod stats;

pub use rusplay_net::{PeerRecord, PeerSecret, Pin};
pub use rusplay_scale::OutputLight;

pub use crate::{
    color::FrameColor,
    error::{ClientError, Stage},
    session::{ClientSession, ClientStream, Credentials, Gpu},
    sink::{FrameSink, LatestFrame, PresentedFrame},
    stats::ClientStats,
};
