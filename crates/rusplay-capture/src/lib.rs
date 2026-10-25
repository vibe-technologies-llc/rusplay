mod error;
mod frame;
mod handoff;
mod modifier;
#[cfg(feature = "pipewire")]
mod pipewire;
mod portal;

#[cfg(feature = "pipewire")]
pub use crate::pipewire::{PipeWireCapture, StreamOptions, default_formats};
pub use crate::{
    error::CaptureError,
    frame::{DrmFourcc, Frame, FrameBuffer, Plane},
    handoff::{Received, Subscriber},
    modifier::{
        DEFAULT_MODIFIERS, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR, FormatModifiers,
        choose_modifier, modifiers_for,
    },
    portal::{PortalSession, PortalStream},
};

pub trait CaptureSource {
    fn next_frame(&mut self) -> Result<Frame, CaptureError>;
}
