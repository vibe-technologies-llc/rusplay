use std::sync::Arc;

use crate::ClipboardError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    ExtDataControl,
    WlrDataControl,
    Portal,
    Focus,
    Memory,
}

impl BackendKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ExtDataControl => "ext-data-control",
            Self::WlrDataControl => "wlr-data-control",
            Self::Portal => "portal",
            Self::Focus => "focus",
            Self::Memory => "memory",
        }
    }
}

pub trait ClipboardBackend: Send {
    fn kind(&self) -> BackendKind;

    fn poll_change(&mut self) -> Result<Option<String>, ClipboardError>;

    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError>;
}

#[derive(Debug)]
pub enum ClipboardState {
    Off,
    Active(BackendKind),
    Unavailable(Arc<ClipboardError>),
}

impl ClipboardState {
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active(_))
    }
}
