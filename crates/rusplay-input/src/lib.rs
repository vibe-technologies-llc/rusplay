pub mod capture;
mod error;
mod event;
#[cfg(feature = "gamepad")]
pub mod gamepad;
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
    event::{GamepadAxis, GamepadButton, GamepadEvent, InputEvent, TouchEvent},
    held::{HeldInput, KeySet, MAX_PADS, MAX_TOUCHES, PAD_AXES, PadState, TouchSet},
    locks::{KEY_CAPS_LOCK, KEY_NUM_LOCK, LOCK_SETTLE, LockMasks, LockState, LockSync, LockTaps},
    record::{InputRecord, StateRecord},
    rumble::{MAX_RUMBLE_EFFECTS, RumbleCommand, RumbleEffect, RumbleTracker},
    sink::SplitSink,
};

pub trait InputSink {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;
}

pub trait InputSource {
    fn poll(&mut self) -> Option<InputEvent>;
}
