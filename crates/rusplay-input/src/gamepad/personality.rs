use rusplay_config::{GamepadPersonality, InputConfig};

use crate::{
    GamepadEvent, InputError, PadFeedback,
    gamepad::{
        pool::{GamepadDevice, GamepadFactory, GamepadPool},
        uhid::UhidGamepad,
        uinput::UinputGamepad,
    },
};

pub enum VirtualGamepad {
    Xbox360(UinputGamepad),
    DualSense(UhidGamepad),
    DualShock4(UhidGamepad),
}

impl GamepadDevice for VirtualGamepad {
    fn handle(&mut self, event: GamepadEvent) -> Result<(), InputError> {
        match self {
            Self::Xbox360(device) => device.handle(event),
            Self::DualSense(device) | Self::DualShock4(device) => device.handle(event),
        }
    }

    fn poll_feedback(&mut self, out: &mut Vec<PadFeedback>) -> Result<(), InputError> {
        match self {
            Self::Xbox360(device) => device.poll_feedback(out),
            Self::DualSense(device) | Self::DualShock4(device) => device.poll_feedback(out),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PersonalityFactory {
    personality: GamepadPersonality,
}

impl PersonalityFactory {
    pub const fn new(personality: GamepadPersonality) -> Self {
        Self { personality }
    }
}

impl GamepadFactory for PersonalityFactory {
    type Device = VirtualGamepad;

    fn create(&mut self, slot: u8) -> Result<VirtualGamepad, InputError> {
        match self.personality {
            GamepadPersonality::Xbox360 => UinputGamepad::create(slot).map(VirtualGamepad::Xbox360),
            GamepadPersonality::DualSense => {
                UhidGamepad::dualsense(slot).map(VirtualGamepad::DualSense)
            }
            GamepadPersonality::DualShock4 => {
                UhidGamepad::dualshock4(slot).map(VirtualGamepad::DualShock4)
            }
        }
    }
}

pub type VirtualGamepads = GamepadPool<PersonalityFactory>;

impl VirtualGamepads {
    pub fn from_config(config: InputConfig) -> Self {
        Self::new(
            PersonalityFactory::new(config.gamepad_personality),
            config.max_gamepads,
        )
    }
}
