use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InputError {
    #[error("the input sink was closed by the compositor")]
    SinkClosed,
    #[error("gamepad slot {0} is outside the supported range")]
    GamepadSlotOutOfRange(u8),
}
