mod backend;
#[cfg(feature = "wayland")]
mod data_control;
mod error;
mod focus;
mod link;
mod memory;
#[cfg(feature = "portal")]
mod portal;
mod sync;
#[cfg(any(feature = "wayland", feature = "portal"))]
mod transfer;

#[cfg(feature = "portal")]
pub use ashpd::desktop::clipboard::Clipboard;

#[cfg(feature = "wayland")]
pub use crate::data_control::{DataControl, DataControlOptions, probe as probe_data_control};
#[cfg(feature = "portal")]
pub use crate::portal::{PortalClipboard, PortalStep, request_clipboard};
pub use crate::{
    backend::{BackendKind, ClipboardBackend, ClipboardState},
    error::ClipboardError,
    focus::{FocusClipboard, FocusClipboardHandle, Offer},
    link::ClipboardLink,
    memory::MemoryClipboard,
    sync::{ClipboardPolicy, ClipboardSync, Disposition},
};
