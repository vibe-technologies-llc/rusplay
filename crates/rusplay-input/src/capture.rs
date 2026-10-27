mod coalesce;
mod core;
mod pad;
mod slots;

pub use self::{
    coalesce::EventCoalescer,
    core::{
        ActivePointer, CaptureCore, CaptureOptions, ClientAction, Hotkeys, StreamRegion, Transition,
    },
    pad::{FaceLayout, PadCaps, PadTranslator, RawAxis, RawEvent},
    slots::PadSlots,
};
