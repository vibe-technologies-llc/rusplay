mod error;
mod frame;
mod handoff;
#[cfg(feature = "pipewire")]
mod pipewire;
mod portal;

#[cfg(feature = "pipewire")]
pub use crate::pipewire::{PipeWireCapture, StreamOptions, default_formats};
pub use crate::{
    error::CaptureError,
    frame::{DrmFourcc, Frame, FrameBuffer, Plane},
    handoff::{Received, Subscriber},
    portal::{PortalSession, PortalStream},
};

pub trait CaptureSource {
    fn next_frame(&mut self) -> Result<Frame, CaptureError>;
}
