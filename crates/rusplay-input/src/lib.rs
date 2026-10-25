mod error;
mod event;
#[cfg(feature = "gamepad")]
pub mod gamepad;
#[cfg(feature = "libei")]
pub mod libei;
mod sink;

pub use crate::{
    error::InputError,
    event::{GamepadAxis, GamepadButton, GamepadEvent, InputEvent},
    sink::SplitSink,
};

pub trait InputSink {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;
}

pub trait InputSource {
    fn poll(&mut self) -> Option<InputEvent>;
}
