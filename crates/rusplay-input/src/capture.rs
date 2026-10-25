mod coalesce;
mod core;
mod pad;
mod slots;

pub use self::{
    coalesce::EventCoalescer,
    core::{CaptureCore, CaptureOptions, StreamRegion, Transition},
    pad::{FaceLayout, PadCaps, PadTranslator, RawAxis, RawEvent},
    slots::PadSlots,
};
