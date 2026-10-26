mod cursor;
mod error;
mod frame;
mod handoff;
mod modifier;
#[cfg(feature = "pipewire")]
mod pipewire;
mod portal;
mod token;

#[cfg(feature = "pipewire")]
pub use crate::pipewire::{PipeWireCapture, StreamOptions, default_formats};
pub use crate::{
    cursor::{
        CursorBitmap, CursorObservation, CursorPixelOrder, CursorPixels, CursorShape,
        CursorTracker, CursorUpdate,
    },
    error::CaptureError,
    frame::{DrmFourcc, Frame, FrameBuffer, Plane},
    handoff::{Publisher, Received, Subscriber, latest},
    modifier::{
        DEFAULT_MODIFIERS, DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR, FormatModifiers,
        choose_modifier, modifiers_for,
    },
    portal::{PortalSession, PortalStream, SharedScreencast, portal_resolution},
    token::{RestoreTokenStore, default_path as default_restore_token_path},
};

pub trait CaptureSource {
    fn next_frame(&mut self) -> Result<Frame, CaptureError>;

    fn cursor_feed(&self) -> Option<Subscriber<CursorUpdate>> {
        None
    }
}
