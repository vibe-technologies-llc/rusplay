mod error;
mod event;

pub use crate::{
    error::InputError,
    event::{GamepadAxis, GamepadButton, GamepadEvent, InputEvent},
};

pub trait InputSink {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;
}

pub trait InputSource {
    fn poll(&mut self) -> Option<InputEvent>;
}
