use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

const GAMEPAD_SLOT_RANGE: RangeInclusive<u8> = 1..=16;
const SENSITIVITY_RANGE: RangeInclusive<f32> = 0.05..=20.0;
const DEADZONE_RANGE: RangeInclusive<f32> = 0.0..=0.9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PointerMode {
    #[default]
    Absolute,
    Relative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseChord {
    CtrlAlt,
    CtrlShift,
    AltShift,
    #[default]
    CtrlAltShift,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct InputConfig {
    pub keyboard: bool,
    pub pointer: bool,
    pub gamepads: bool,
    pub max_gamepads: u8,
    pub pointer_mode: PointerMode,
    pub pointer_sensitivity: f32,
    pub gamepad_deadzone: f32,
    pub release_chord: ReleaseChord,
    pub rumble_max_ms: u16,
    pub resend_delay_ms: u16,
    pub snapshot_interval_ms: u16,
    pub accept_remote: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            keyboard: true,
            pointer: true,
            gamepads: true,
            max_gamepads: 4,
            pointer_mode: PointerMode::default(),
            pointer_sensitivity: 1.0,
            gamepad_deadzone: 0.05,
            release_chord: ReleaseChord::default(),
            rumble_max_ms: 5000,
            resend_delay_ms: 8,
            snapshot_interval_ms: 1000,
            accept_remote: true,
        }
    }
}

impl InputConfig {
    pub(crate) fn validate(self) -> Result<(), ConfigError> {
        if !GAMEPAD_SLOT_RANGE.contains(&self.max_gamepads) {
            return Err(ConfigError::GamepadSlotsOutOfRange(self.max_gamepads));
        }

        if !SENSITIVITY_RANGE.contains(&self.pointer_sensitivity) {
            return Err(ConfigError::PointerSensitivityOutOfRange(
                self.pointer_sensitivity,
            ));
        }

        if !DEADZONE_RANGE.contains(&self.gamepad_deadzone) {
            return Err(ConfigError::GamepadDeadzoneOutOfRange(
                self.gamepad_deadzone,
            ));
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

    #[test]
    fn pointer_and_gamepad_tuning_must_be_in_range() {
        let slow = InputConfig {
            pointer_sensitivity: 0.0,
            ..InputConfig::default()
        };
        let not_a_number = InputConfig {
            pointer_sensitivity: f32::NAN,
            ..InputConfig::default()
        };
        let dead = InputConfig {
            gamepad_deadzone: 1.0,
            ..InputConfig::default()
        };

        assert!(matches!(
            slow.validate(),
            Err(ConfigError::PointerSensitivityOutOfRange(_))
        ));
        assert!(matches!(
            not_a_number.validate(),
            Err(ConfigError::PointerSensitivityOutOfRange(_))
        ));
        assert!(matches!(
            dead.validate(),
            Err(ConfigError::GamepadDeadzoneOutOfRange(_))
        ));
    }

    #[test]
    fn snapshot_and_remote_input_defaults_ship_on() {
        let config: InputConfig = toml::from_str("").unwrap();

        assert_eq!(config.snapshot_interval_ms, 1000);
        assert!(config.accept_remote);
    }

    #[test]
    fn snapshot_and_remote_input_can_be_turned_off() {
        let config: InputConfig =
            toml::from_str("snapshot_interval_ms = 0\naccept_remote = false").unwrap();

        assert_eq!(config.snapshot_interval_ms, 0);
        assert!(!config.accept_remote);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn release_chord_uses_kebab_case_names() {
        let config: InputConfig = toml::from_str("release_chord = \"ctrl-alt\"").unwrap();

        assert_eq!(config.release_chord, ReleaseChord::CtrlAlt);
        assert_eq!(
            InputConfig::default().release_chord,
            ReleaseChord::CtrlAltShift
        );
    }
}
