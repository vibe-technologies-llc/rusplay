pub mod capture;
mod error;
mod event;
mod feedback;
#[cfg(feature = "gamepad")]
pub mod gamepad;
#[cfg(all(test, feature = "gamepad"))]
mod hardware;
mod held;
#[cfg(feature = "libei")]
pub mod libei;
mod locks;
mod record;
mod rumble;
mod sink;
#[cfg(feature = "wayland")]
pub mod wayland;
mod wire;

pub use crate::{
    error::InputError,
    event::{GamepadAxis, GamepadButton, GamepadEvent, InputEvent, SensorKind, TouchEvent},
    feedback::{FeedbackOutcome, PadFeedback, TRIGGER_EFFECT_LEN},
    held::{
        HeldInput, KeySet, MAX_PAD_FINGERS, MAX_PADS, MAX_TOUCHES, PAD_AXES, PadFingers, PadState,
        TouchSet,
    },
    locks::{KEY_CAPS_LOCK, KEY_NUM_LOCK, LOCK_SETTLE, LockMasks, LockState, LockSync, LockTaps},
    record::{InputRecord, StateRecord},
    rumble::{MAX_RUMBLE_EFFECTS, RumbleCommand, RumbleEffect, RumbleTracker},
    sink::SplitSink,
};

pub trait InputSink {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;

    fn inject_batch(&mut self, events: &[InputEvent], results: &mut Vec<Result<(), InputError>>) {
        results.clear();
        results.extend(events.iter().map(|event| self.inject(*event)));
    }
}

pub trait InputSource {
    fn poll(&mut self) -> Option<InputEvent>;
}
