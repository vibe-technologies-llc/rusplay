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
    Auto,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChordModifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl ChordModifiers {
    pub const fn includes(self, other: Self) -> bool {
        (self.ctrl || !other.ctrl) && (self.shift || !other.shift) && (self.alt || !other.alt)
    }
}

impl ReleaseChord {
    pub const fn modifiers(self) -> ChordModifiers {
        let (ctrl, shift, alt) = match self {
            Self::CtrlAlt => (true, false, true),
            Self::CtrlShift => (true, true, false),
            Self::AltShift => (false, true, true),
            Self::CtrlAltShift => (true, true, true),
        };

        ChordModifiers { ctrl, shift, alt }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HotkeyKey {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct InputConfig {
    pub keyboard: bool,
    pub pointer: bool,
    pub touch: bool,
    pub gamepads: bool,
    pub max_gamepads: u8,
    pub pointer_mode: PointerMode,
    pub pointer_auto_delay_ms: u16,
    pub pointer_sensitivity: f32,
    pub gamepad_deadzone: f32,
    pub release_chord: ReleaseChord,
    pub hotkeys: bool,
    pub hotkey_chord: ReleaseChord,
    pub hotkey_fullscreen: HotkeyKey,
    pub hotkey_stats: HotkeyKey,
    pub hotkey_disconnect: HotkeyKey,
    pub hotkey_pointer_mode: HotkeyKey,
    pub inhibit_shortcuts: bool,
    pub sync_locks: bool,
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
            touch: true,
            gamepads: true,
            max_gamepads: 4,
            pointer_mode: PointerMode::default(),
            pointer_auto_delay_ms: 250,
            pointer_sensitivity: 1.0,
            gamepad_deadzone: 0.05,
            release_chord: ReleaseChord::default(),
            hotkeys: true,
            hotkey_chord: ReleaseChord::CtrlAlt,
            hotkey_fullscreen: HotkeyKey::F,
            hotkey_stats: HotkeyKey::S,
            hotkey_disconnect: HotkeyKey::D,
            hotkey_pointer_mode: HotkeyKey::P,
            inhibit_shortcuts: true,
            sync_locks: true,
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

        if self.hotkeys {
            self.validate_hotkeys()?;
        }

        Ok(())
    }

    fn validate_hotkeys(self) -> Result<(), ConfigError> {
        if self
            .hotkey_chord
            .modifiers()
            .includes(self.release_chord.modifiers())
        {
            return Err(ConfigError::HotkeyChordShadowsRelease {
                hotkey: self.hotkey_chord,
                release: self.release_chord,
            });
        }

        let bound = [
            ("input.hotkey_fullscreen", self.hotkey_fullscreen),
            ("input.hotkey_stats", self.hotkey_stats),
            ("input.hotkey_disconnect", self.hotkey_disconnect),
            ("input.hotkey_pointer_mode", self.hotkey_pointer_mode),
        ];
        for (position, (field, key)) in bound.iter().enumerate() {
            if bound[..position].iter().any(|(_, earlier)| earlier == key) {
                return Err(ConfigError::HotkeyKeyReused { key: *key, field });
            }
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
    fn pointer_mode_accepts_auto_with_a_default_delay() {
        let config: InputConfig = toml::from_str("pointer_mode = \"auto\"").unwrap();

        assert_eq!(config.pointer_mode, PointerMode::Auto);
        assert_eq!(config.pointer_auto_delay_ms, 250);
        assert_eq!(InputConfig::default().pointer_mode, PointerMode::Absolute);
    }

    #[test]
    fn touch_ships_on_and_can_be_turned_off() {
        let default: InputConfig = toml::from_str("").unwrap();
        let off: InputConfig = toml::from_str("touch = false").unwrap();

        assert!(default.touch);
        assert!(!off.touch);
    }

    #[test]
    fn lock_sync_ships_on_and_can_be_turned_off() {
        let default: InputConfig = toml::from_str("").unwrap();
        let off: InputConfig = toml::from_str("sync_locks = false").unwrap();

        assert!(default.sync_locks);
        assert!(!off.sync_locks);
    }

    #[test]
    fn shortcut_inhibition_ships_on_and_can_be_turned_off() {
        let default: InputConfig = toml::from_str("").unwrap();
        let off: InputConfig = toml::from_str("inhibit_shortcuts = false").unwrap();

        assert!(default.inhibit_shortcuts);
        assert!(!off.inhibit_shortcuts);
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

    #[test]
    fn hotkeys_ship_on_with_distinct_keys_under_a_chord_that_leaves_the_release_chord_alone() {
        let config = InputConfig::default();

        assert!(config.hotkeys);
        assert!(config.validate().is_ok());
        assert!(
            !config
                .hotkey_chord
                .modifiers()
                .includes(config.release_chord.modifiers())
        );
    }

    #[test]
    fn hotkey_keys_use_lowercase_names() {
        let config: InputConfig =
            toml::from_str("hotkey_fullscreen = \"f11\"\nhotkey_stats = \"x\"").unwrap();

        assert_eq!(config.hotkey_fullscreen, HotkeyKey::F11);
        assert_eq!(config.hotkey_stats, HotkeyKey::X);
    }

    #[test]
    fn a_hotkey_chord_containing_the_release_chord_is_rejected() {
        let same = InputConfig {
            release_chord: ReleaseChord::CtrlAlt,
            ..InputConfig::default()
        };
        let wider = InputConfig {
            hotkey_chord: ReleaseChord::CtrlAltShift,
            release_chord: ReleaseChord::CtrlShift,
            ..InputConfig::default()
        };
        let off = InputConfig {
            hotkeys: false,
            ..same
        };

        assert!(matches!(
            same.validate(),
            Err(ConfigError::HotkeyChordShadowsRelease { .. })
        ));
        assert!(matches!(
            wider.validate(),
            Err(ConfigError::HotkeyChordShadowsRelease { .. })
        ));
        assert!(off.validate().is_ok());
    }

    #[test]
    fn one_key_cannot_serve_two_actions() {
        let config = InputConfig {
            hotkey_disconnect: HotkeyKey::S,
            ..InputConfig::default()
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::HotkeyKeyReused {
                key: HotkeyKey::S,
                field: "input.hotkey_disconnect"
            })
        ));
    }

    #[test]
    fn chord_modifiers_include_each_other_by_subset() {
        let ctrl_alt = ReleaseChord::CtrlAlt.modifiers();
        let all = ReleaseChord::CtrlAltShift.modifiers();
        let alt_shift = ReleaseChord::AltShift.modifiers();

        assert!(all.includes(ctrl_alt));
        assert!(ctrl_alt.includes(ctrl_alt));
        assert!(!ctrl_alt.includes(all));
        assert!(!ctrl_alt.includes(alt_shift));
    }
}
