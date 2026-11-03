mod coalesce;
mod core;
mod pad;
mod sensor;
mod sibling;
mod slots;

pub use self::{
    coalesce::EventCoalescer,
    core::{
        ActivePointer, CaptureCore, CaptureOptions, ClientAction, Hotkeys, StreamRegion, Transition,
    },
    pad::{FaceLayout, PadCaps, PadTranslator, RawAxis, RawEvent},
    sensor::{MotionCaps, MotionTranslator, SiblingTranslator, TouchpadRange, TouchpadTranslator},
    sibling::{
        NodeCaps, NodeDescription, NodeIdentity, NodeRole, PairedPad, non_empty, pair_siblings,
    },
    slots::PadSlots,
};
