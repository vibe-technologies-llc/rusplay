use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

const GAMEPAD_SLOT_RANGE: RangeInclusive<u8> = 1..=16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PointerMode {
    #[default]
    Absolute,
    Relative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InputConfig {
    pub keyboard: bool,
    pub pointer: bool,
    pub gamepads: bool,
    pub max_gamepads: u8,
    pub pointer_mode: PointerMode,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            keyboard: true,
            pointer: true,
            gamepads: true,
            max_gamepads: 4,
            pointer_mode: PointerMode::default(),
        }
    }
}

impl InputConfig {
    pub(crate) fn validate(self) -> Result<(), ConfigError> {
        if !GAMEPAD_SLOT_RANGE.contains(&self.max_gamepads) {
            return Err(ConfigError::GamepadSlotsOutOfRange(self.max_gamepads));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamepad_slot_count_must_be_in_range() {
        let none = InputConfig {
            max_gamepads: 0,
            ..InputConfig::default()
        };
        let many = InputConfig {
            max_gamepads: 17,
            ..InputConfig::default()
        };

        assert!(matches!(
            none.validate(),
            Err(ConfigError::GamepadSlotsOutOfRange(0))
        ));
        assert!(matches!(
            many.validate(),
            Err(ConfigError::GamepadSlotsOutOfRange(17))
        ));
        assert!(InputConfig::default().validate().is_ok());
    }
}
