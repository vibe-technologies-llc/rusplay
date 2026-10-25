pub mod capture;
mod error;
mod event;
#[cfg(feature = "gamepad")]
pub mod gamepad;
#[cfg(feature = "libei")]
pub mod libei;
mod rumble;
mod sink;
#[cfg(feature = "wayland")]
pub mod wayland;
mod wire;

pub use crate::{
    error::InputError,
    event::{GamepadAxis, GamepadButton, GamepadEvent, InputEvent},
    rumble::{MAX_RUMBLE_EFFECTS, RumbleCommand, RumbleEffect, RumbleTracker},
    sink::SplitSink,
};

pub trait InputSink {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;
}

pub trait InputSource {
    fn poll(&mut self) -> Option<InputEvent>;
}
