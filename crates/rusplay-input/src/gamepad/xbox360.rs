use evdev::{AbsoluteAxisCode, KeyCode};

use crate::{GamepadAxis, GamepadButton, GamepadEvent, InputError};

pub(super) const STICK_MIN: i32 = -32768;
pub(super) const STICK_MAX: i32 = 32767;
pub(super) const TRIGGER_MAX: i32 = 255;
const STICK_NEGATIVE_SCALE: f32 = 32768.0;
const STICK_POSITIVE_SCALE: f32 = 32767.0;
const TRIGGER_SCALE: f32 = 255.0;
pub(super) const HAT_MAX: i32 = 1;

const XBOX_X: KeyCode = KeyCode::BTN_NORTH;
const XBOX_Y: KeyCode = KeyCode::BTN_WEST;

pub(super) const KEYS: [KeyCode; 11] = [
    KeyCode::BTN_SOUTH,
    KeyCode::BTN_EAST,
    XBOX_X,
    XBOX_Y,
    KeyCode::BTN_TL,
    KeyCode::BTN_TR,
    KeyCode::BTN_SELECT,
    KeyCode::BTN_START,
    KeyCode::BTN_MODE,
    KeyCode::BTN_THUMBL,
    KeyCode::BTN_THUMBR,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Key { code: u16, value: i32 },
    Abs { code: u16, value: i32 },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Dpad(u8);

impl Dpad {
    const UP: u8 = 1;
    const DOWN: u8 = 2;
    const LEFT: u8 = 4;
    const RIGHT: u8 = 8;

    fn set(&mut self, direction: u8, pressed: bool) {
        if pressed {
            self.0 |= direction;
        } else {
            self.0 &= !direction;
        }
    }

    fn held(self, direction: u8) -> i32 {
        i32::from(self.0 & direction != 0)
    }

    fn x(self) -> i32 {
        self.held(Self::RIGHT) - self.held(Self::LEFT)
    }

    fn y(self) -> i32 {
        self.held(Self::DOWN) - self.held(Self::UP)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Xbox360Mapper {
    dpad: Dpad,
}

impl Xbox360Mapper {
    pub fn map(&mut self, event: GamepadEvent) -> Result<Option<Report>, InputError> {
        match event {
            GamepadEvent::Connected | GamepadEvent::Disconnected => Ok(None),
            GamepadEvent::Button { button, pressed } => self.map_button(button, pressed).map(Some),
            GamepadEvent::Axis { axis, value } => map_axis(axis, value).map(Some),
        }
    }

    fn map_button(&mut self, button: GamepadButton, pressed: bool) -> Result<Report, InputError> {
        let dpad = &mut self.dpad;
        match button {
            GamepadButton::DPAD_UP => dpad.set(Dpad::UP, pressed),
            GamepadButton::DPAD_DOWN => dpad.set(Dpad::DOWN, pressed),
            GamepadButton::DPAD_LEFT => dpad.set(Dpad::LEFT, pressed),
            GamepadButton::DPAD_RIGHT => dpad.set(Dpad::RIGHT, pressed),
            other => {
                return Ok(Report::Key {
                    code: key_for(other)?.0,
                    value: i32::from(pressed),
                });
            }
        }

        let (axis, value) = match button {
            GamepadButton::DPAD_LEFT | GamepadButton::DPAD_RIGHT => {
                (AbsoluteAxisCode::ABS_HAT0X, dpad.x())
            }
            _ => (AbsoluteAxisCode::ABS_HAT0Y, dpad.y()),
        };

        Ok(Report::Abs {
            code: axis.0,
            value,
        })
    }
}

fn key_for(button: GamepadButton) -> Result<KeyCode, InputError> {
    match button {
        GamepadButton::SOUTH => Ok(KeyCode::BTN_SOUTH),
        GamepadButton::EAST => Ok(KeyCode::BTN_EAST),
        GamepadButton::WEST => Ok(XBOX_X),
        GamepadButton::NORTH => Ok(XBOX_Y),
        GamepadButton::BACK => Ok(KeyCode::BTN_SELECT),
        GamepadButton::GUIDE => Ok(KeyCode::BTN_MODE),
        GamepadButton::START => Ok(KeyCode::BTN_START),
        GamepadButton::LEFT_STICK => Ok(KeyCode::BTN_THUMBL),
        GamepadButton::RIGHT_STICK => Ok(KeyCode::BTN_THUMBR),
        GamepadButton::LEFT_SHOULDER => Ok(KeyCode::BTN_TL),
        GamepadButton::RIGHT_SHOULDER => Ok(KeyCode::BTN_TR),
        GamepadButton(unknown) => Err(InputError::GamepadButtonUnknown(unknown)),
    }
}

fn map_axis(axis: GamepadAxis, value: f32) -> Result<Report, InputError> {
    if !value.is_finite() {
        return Err(InputError::NonFiniteValue);
    }

    let (code, scaled) = match axis {
        GamepadAxis::LEFT_X => (AbsoluteAxisCode::ABS_X, stick(value)),
        GamepadAxis::LEFT_Y => (AbsoluteAxisCode::ABS_Y, stick(value)),
        GamepadAxis::RIGHT_X => (AbsoluteAxisCode::ABS_RX, stick(value)),
        GamepadAxis::RIGHT_Y => (AbsoluteAxisCode::ABS_RY, stick(value)),
        GamepadAxis::LEFT_TRIGGER => (AbsoluteAxisCode::ABS_Z, trigger(value)),
        GamepadAxis::RIGHT_TRIGGER => (AbsoluteAxisCode::ABS_RZ, trigger(value)),
        GamepadAxis(unknown) => return Err(InputError::GamepadAxisUnknown(unknown)),
    };

    Ok(Report::Abs {
        code: code.0,
        value: scaled,
    })
}

#[allow(clippy::cast_possible_truncation)]
fn stick(value: f32) -> i32 {
    let clamped = value.clamp(-1.0, 1.0);
    if clamped < 0.0 {
        (clamped * STICK_NEGATIVE_SCALE).round() as i32
    } else {
        (clamped * STICK_POSITIVE_SCALE).round() as i32
    }
}

#[allow(clippy::cast_possible_truncation)]
fn trigger(value: f32) -> i32 {
    (value.clamp(0.0, 1.0) * TRIGGER_SCALE).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(code: AbsoluteAxisCode, value: i32) -> Report {
        Report::Abs {
            code: code.0,
            value,
        }
    }

    fn key(code: KeyCode, value: i32) -> Report {
        Report::Key {
            code: code.0,
            value,
        }
    }

    fn press(mapper: &mut Xbox360Mapper, button: GamepadButton, pressed: bool) -> Report {
        mapper
            .map(GamepadEvent::Button { button, pressed })
            .unwrap()
            .unwrap()
    }

    fn axis(mapper: &mut Xbox360Mapper, axis: GamepadAxis, value: f32) -> Report {
        mapper
            .map(GamepadEvent::Axis { axis, value })
            .unwrap()
            .unwrap()
    }

    #[test]
    fn face_buttons_follow_the_xpad_layout() {
        let mut mapper = Xbox360Mapper::default();

        assert_eq!(
            press(&mut mapper, GamepadButton::SOUTH, true),
            key(KeyCode::BTN_SOUTH, 1)
        );
        assert_eq!(
            press(&mut mapper, GamepadButton::EAST, false),
            key(KeyCode::BTN_EAST, 0)
        );
        assert_eq!(
            press(&mut mapper, GamepadButton::WEST, true),
            key(KeyCode(0x133), 1)
        );
        assert_eq!(
            press(&mut mapper, GamepadButton::NORTH, true),
            key(KeyCode(0x134), 1)
        );
    }

    #[test]
    fn menu_stick_and_shoulder_buttons_map_to_their_codes() {
        let mut mapper = Xbox360Mapper::default();

        let cases = [
            (GamepadButton::BACK, KeyCode::BTN_SELECT),
            (GamepadButton::START, KeyCode::BTN_START),
            (GamepadButton::GUIDE, KeyCode::BTN_MODE),
            (GamepadButton::LEFT_STICK, KeyCode::BTN_THUMBL),
            (GamepadButton::RIGHT_STICK, KeyCode::BTN_THUMBR),
            (GamepadButton::LEFT_SHOULDER, KeyCode::BTN_TL),
            (GamepadButton::RIGHT_SHOULDER, KeyCode::BTN_TR),
        ];

        for (button, code) in cases {
            assert_eq!(press(&mut mapper, button, true), key(code, 1));
        }
    }

    #[test]
    fn every_mapped_button_is_declared_on_the_device() {
        let mut mapper = Xbox360Mapper::default();

        for id in 0..=10 {
            let Report::Key { code, .. } = press(&mut mapper, GamepadButton(id), true) else {
                panic!("button {id} did not produce a key");
            };

            assert!(KEYS.contains(&KeyCode(code)));
        }
    }

    #[test]
    fn unknown_button_and_axis_are_errors() {
        let mut mapper = Xbox360Mapper::default();

        let button = mapper.map(GamepadEvent::Button {
            button: GamepadButton(99),
            pressed: true,
        });
        let axis = mapper.map(GamepadEvent::Axis {
            axis: GamepadAxis(42),
            value: 0.0,
        });

        assert!(matches!(button, Err(InputError::GamepadButtonUnknown(99))));
        assert!(matches!(axis, Err(InputError::GamepadAxisUnknown(42))));
    }

    #[test]
    fn sticks_scale_to_the_signed_16_bit_range() {
        let mut mapper = Xbox360Mapper::default();

        assert_eq!(
            axis(&mut mapper, GamepadAxis::LEFT_X, 1.0),
            abs(AbsoluteAxisCode::ABS_X, 32767)
        );
        assert_eq!(
            axis(&mut mapper, GamepadAxis::LEFT_Y, -1.0),
            abs(AbsoluteAxisCode::ABS_Y, -32768)
        );
        assert_eq!(
            axis(&mut mapper, GamepadAxis::RIGHT_X, 0.0),
            abs(AbsoluteAxisCode::ABS_RX, 0)
        );
        assert_eq!(
            axis(&mut mapper, GamepadAxis::RIGHT_Y, 0.5),
            abs(AbsoluteAxisCode::ABS_RY, 16384)
        );
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let mut mapper = Xbox360Mapper::default();

        assert_eq!(
            axis(&mut mapper, GamepadAxis::LEFT_X, 7.0),
            abs(AbsoluteAxisCode::ABS_X, 32767)
        );
        assert_eq!(
            axis(&mut mapper, GamepadAxis::LEFT_TRIGGER, -3.0),
            abs(AbsoluteAxisCode::ABS_Z, 0)
        );
        assert_eq!(
            axis(&mut mapper, GamepadAxis::RIGHT_TRIGGER, 9.0),
            abs(AbsoluteAxisCode::ABS_RZ, 255)
        );
    }

    #[test]
    fn triggers_scale_to_eight_bits() {
        let mut mapper = Xbox360Mapper::default();

        assert_eq!(
            axis(&mut mapper, GamepadAxis::LEFT_TRIGGER, 1.0),
            abs(AbsoluteAxisCode::ABS_Z, 255)
        );
        assert_eq!(
            axis(&mut mapper, GamepadAxis::RIGHT_TRIGGER, 0.5),
            abs(AbsoluteAxisCode::ABS_RZ, 128)
        );
    }

    #[test]
    fn non_finite_axis_values_are_rejected() {
        let mut mapper = Xbox360Mapper::default();

        let result = mapper.map(GamepadEvent::Axis {
            axis: GamepadAxis::LEFT_X,
            value: f32::NAN,
        });

        assert!(matches!(result, Err(InputError::NonFiniteValue)));
    }

    #[test]
    fn dpad_buttons_drive_the_hat_axes() {
        let mut mapper = Xbox360Mapper::default();

        assert_eq!(
            press(&mut mapper, GamepadButton::DPAD_UP, true),
            abs(AbsoluteAxisCode::ABS_HAT0Y, -1)
        );
        assert_eq!(
            press(&mut mapper, GamepadButton::DPAD_RIGHT, true),
            abs(AbsoluteAxisCode::ABS_HAT0X, 1)
        );
        assert_eq!(
            press(&mut mapper, GamepadButton::DPAD_DOWN, true),
            abs(AbsoluteAxisCode::ABS_HAT0Y, 0)
        );
        assert_eq!(
            press(&mut mapper, GamepadButton::DPAD_DOWN, false),
            abs(AbsoluteAxisCode::ABS_HAT0Y, -1)
        );
        assert_eq!(
            press(&mut mapper, GamepadButton::DPAD_UP, false),
            abs(AbsoluteAxisCode::ABS_HAT0Y, 0)
        );
        assert_eq!(
            press(&mut mapper, GamepadButton::DPAD_RIGHT, false),
            abs(AbsoluteAxisCode::ABS_HAT0X, 0)
        );
    }

    #[test]
    fn connection_events_produce_no_report() {
        let mut mapper = Xbox360Mapper::default();

        assert_eq!(mapper.map(GamepadEvent::Connected).unwrap(), None);
        assert_eq!(mapper.map(GamepadEvent::Disconnected).unwrap(), None);
    }
}
