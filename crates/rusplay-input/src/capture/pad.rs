use crate::{GamepadAxis, GamepadButton, GamepadEvent, InputEvent};

const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_NORTH: u16 = 0x133;
const BTN_WEST: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_TL2: u16 = 0x138;
const BTN_TR2: u16 = 0x139;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;
const BTN_DPAD_UP: u16 = 0x220;
const BTN_DPAD_DOWN: u16 = 0x221;
const BTN_DPAD_LEFT: u16 = 0x222;
const BTN_DPAD_RIGHT: u16 = 0x223;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

const AXIS_COUNT: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawAxis {
    pub min: i32,
    pub max: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceLayout {
    Xpad,
    Positional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadCaps {
    pub layout: FaceLayout,
    pub axes: Vec<(u16, RawAxis)>,
}

impl PadCaps {
    fn range(&self, code: u16) -> Option<RawAxis> {
        self.axes
            .iter()
            .find_map(|&(axis, range)| (axis == code).then_some(range))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawEvent {
    Key { code: u16, value: i32 },
    Abs { code: u16, value: i32 },
}

#[derive(Debug, Clone)]
pub struct PadTranslator {
    caps: PadCaps,
    deadzone: f32,
    trigger_buttons: bool,
    last: [Option<f32>; AXIS_COUNT],
    hat: (i32, i32),
}

impl PadTranslator {
    pub fn new(caps: PadCaps, deadzone: f32) -> Self {
        let trigger_buttons = caps.range(ABS_Z).is_none() && caps.range(ABS_RZ).is_none();

        Self {
            caps,
            deadzone: deadzone.clamp(0.0, 0.99),
            trigger_buttons,
            last: [None; AXIS_COUNT],
            hat: (0, 0),
        }
    }

    pub fn translate(&mut self, slot: u8, raw: RawEvent, out: &mut Vec<InputEvent>) {
        let mut events = Vec::new();
        match raw {
            RawEvent::Key { code, value } => self.key(code, value, &mut events),
            RawEvent::Abs { code, value } => self.abs(code, value, &mut events),
        }

        out.extend(
            events
                .into_iter()
                .map(|event| InputEvent::Gamepad { slot, event }),
        );
    }

    fn key(&mut self, code: u16, value: i32, out: &mut Vec<GamepadEvent>) {
        if value > 1 {
            return;
        }
        let pressed = value == 1;

        if self.trigger_buttons {
            let trigger = match code {
                BTN_TL2 => Some(GamepadAxis::LEFT_TRIGGER),
                BTN_TR2 => Some(GamepadAxis::RIGHT_TRIGGER),
                _ => None,
            };
            if let Some(axis) = trigger {
                let value = if pressed { 1.0 } else { 0.0 };
                self.emit_axis(axis, value, out);
                return;
            }
        }

        if let Some(button) = button_for(code, self.caps.layout) {
            out.push(GamepadEvent::Button { button, pressed });
        }
    }

    fn abs(&mut self, code: u16, value: i32, out: &mut Vec<GamepadEvent>) {
        match code {
            ABS_HAT0X => self.hat_x(value, out),
            ABS_HAT0Y => self.hat_y(value, out),
            ABS_X | ABS_Y | ABS_RX | ABS_RY => {
                if let (Some(range), Some(axis)) = (self.caps.range(code), stick_axis(code)) {
                    let normalised = self.dead(centred(value, range));
                    self.emit_axis(axis, normalised, out);
                }
            }
            ABS_Z | ABS_RZ => {
                if let (Some(range), Some(axis)) = (self.caps.range(code), trigger_axis(code)) {
                    self.emit_axis(axis, unsigned(value, range), out);
                }
            }
            _ => {}
        }
    }

    fn hat_x(&mut self, value: i32, out: &mut Vec<GamepadEvent>) {
        let value = value.signum();
        let previous = std::mem::replace(&mut self.hat.0, value);
        dpad_change(
            previous,
            value,
            GamepadButton::DPAD_LEFT,
            GamepadButton::DPAD_RIGHT,
            out,
        );
    }

    fn hat_y(&mut self, value: i32, out: &mut Vec<GamepadEvent>) {
        let value = value.signum();
        let previous = std::mem::replace(&mut self.hat.1, value);
        dpad_change(
            previous,
            value,
            GamepadButton::DPAD_UP,
            GamepadButton::DPAD_DOWN,
            out,
        );
    }

    fn dead(&self, value: f32) -> f32 {
        let magnitude = value.abs();
        if magnitude <= self.deadzone {
            0.0
        } else {
            value.signum() * (magnitude - self.deadzone) / (1.0 - self.deadzone)
        }
    }

    fn emit_axis(&mut self, axis: GamepadAxis, value: f32, out: &mut Vec<GamepadEvent>) {
        let Some(last) = self.last.get_mut(usize::from(axis.0)) else {
            return;
        };

        if *last != Some(value) {
            *last = Some(value);
            out.push(GamepadEvent::Axis { axis, value });
        }
    }
}

fn dpad_change(
    previous: i32,
    value: i32,
    negative: GamepadButton,
    positive: GamepadButton,
    out: &mut Vec<GamepadEvent>,
) {
    if previous == value {
        return;
    }

    for (direction, button) in [(-1, negative), (1, positive)] {
        if previous == direction {
            out.push(GamepadEvent::Button {
                button,
                pressed: false,
            });
        }
        if value == direction {
            out.push(GamepadEvent::Button {
                button,
                pressed: true,
            });
        }
    }
}

fn stick_axis(code: u16) -> Option<GamepadAxis> {
    match code {
        ABS_X => Some(GamepadAxis::LEFT_X),
        ABS_Y => Some(GamepadAxis::LEFT_Y),
        ABS_RX => Some(GamepadAxis::RIGHT_X),
        ABS_RY => Some(GamepadAxis::RIGHT_Y),
        _ => None,
    }
}

fn trigger_axis(code: u16) -> Option<GamepadAxis> {
    match code {
        ABS_Z => Some(GamepadAxis::LEFT_TRIGGER),
        ABS_RZ => Some(GamepadAxis::RIGHT_TRIGGER),
        _ => None,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn centred(value: i32, range: RawAxis) -> f32 {
    let centre = f64::midpoint(f64::from(range.min), f64::from(range.max));
    let half = (f64::from(range.max) - f64::from(range.min)) / 2.0;
    if half <= 0.0 {
        return 0.0;
    }

    ((f64::from(value) - centre) / half).clamp(-1.0, 1.0) as f32
}

#[allow(clippy::cast_possible_truncation)]
fn unsigned(value: i32, range: RawAxis) -> f32 {
    let span = f64::from(range.max) - f64::from(range.min);
    if span <= 0.0 {
        return 0.0;
    }

    ((f64::from(value) - f64::from(range.min)) / span).clamp(0.0, 1.0) as f32
}

fn button_for(code: u16, layout: FaceLayout) -> Option<GamepadButton> {
    let (north, west) = match layout {
        FaceLayout::Xpad => (GamepadButton::WEST, GamepadButton::NORTH),
        FaceLayout::Positional => (GamepadButton::NORTH, GamepadButton::WEST),
    };

    Some(match code {
        BTN_SOUTH => GamepadButton::SOUTH,
        BTN_EAST => GamepadButton::EAST,
        BTN_NORTH => north,
        BTN_WEST => west,
        BTN_TL => GamepadButton::LEFT_SHOULDER,
        BTN_TR => GamepadButton::RIGHT_SHOULDER,
        BTN_SELECT => GamepadButton::BACK,
        BTN_START => GamepadButton::START,
        BTN_MODE => GamepadButton::GUIDE,
        BTN_THUMBL => GamepadButton::LEFT_STICK,
        BTN_THUMBR => GamepadButton::RIGHT_STICK,
        BTN_DPAD_UP => GamepadButton::DPAD_UP,
        BTN_DPAD_DOWN => GamepadButton::DPAD_DOWN,
        BTN_DPAD_LEFT => GamepadButton::DPAD_LEFT,
        BTN_DPAD_RIGHT => GamepadButton::DPAD_RIGHT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STICK: RawAxis = RawAxis {
        min: -32768,
        max: 32767,
    };
    const TRIGGER: RawAxis = RawAxis { min: 0, max: 255 };

    fn caps(layout: FaceLayout) -> PadCaps {
        PadCaps {
            layout,
            axes: vec![
                (ABS_X, STICK),
                (ABS_Y, STICK),
                (ABS_RX, STICK),
                (ABS_RY, STICK),
                (ABS_Z, TRIGGER),
                (ABS_RZ, TRIGGER),
            ],
        }
    }

    fn translator(deadzone: f32) -> PadTranslator {
        PadTranslator::new(caps(FaceLayout::Xpad), deadzone)
    }

    fn run(translator: &mut PadTranslator, raw: RawEvent) -> Vec<GamepadEvent> {
        let mut out = Vec::new();
        translator.translate(2, raw, &mut out);

        out.into_iter()
            .map(|event| match event {
                InputEvent::Gamepad { slot: 2, event } => event,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    fn pressed(button: GamepadButton, pressed: bool) -> GamepadEvent {
        GamepadEvent::Button { button, pressed }
    }

    fn axis_value(events: &[GamepadEvent]) -> f32 {
        match events {
            [GamepadEvent::Axis { value, .. }] => *value,
            other => panic!("expected one axis event, got {other:?}"),
        }
    }

    #[test]
    fn xpad_face_buttons_invert_the_host_mapping() {
        let mut pad = translator(0.0);

        assert_eq!(
            run(
                &mut pad,
                RawEvent::Key {
                    code: BTN_SOUTH,
                    value: 1
                }
            ),
            vec![pressed(GamepadButton::SOUTH, true)]
        );
        assert_eq!(
            run(
                &mut pad,
                RawEvent::Key {
                    code: BTN_NORTH,
                    value: 1
                }
            ),
            vec![pressed(GamepadButton::WEST, true)]
        );
        assert_eq!(
            run(
                &mut pad,
                RawEvent::Key {
                    code: BTN_WEST,
                    value: 0
                }
            ),
            vec![pressed(GamepadButton::NORTH, false)]
        );
    }

    #[test]
    fn positional_layout_keeps_the_physical_positions() {
        let mut pad = PadTranslator::new(caps(FaceLayout::Positional), 0.0);

        assert_eq!(
            run(
                &mut pad,
                RawEvent::Key {
                    code: BTN_NORTH,
                    value: 1
                }
            ),
            vec![pressed(GamepadButton::NORTH, true)]
        );
        assert_eq!(
            run(
                &mut pad,
                RawEvent::Key {
                    code: BTN_WEST,
                    value: 1
                }
            ),
            vec![pressed(GamepadButton::WEST, true)]
        );
    }

    #[test]
    fn the_whole_button_set_is_mapped_and_repeats_are_ignored() {
        let mut pad = translator(0.0);
        let codes = [
            (BTN_TL, GamepadButton::LEFT_SHOULDER),
            (BTN_TR, GamepadButton::RIGHT_SHOULDER),
            (BTN_SELECT, GamepadButton::BACK),
            (BTN_START, GamepadButton::START),
            (BTN_MODE, GamepadButton::GUIDE),
            (BTN_THUMBL, GamepadButton::LEFT_STICK),
            (BTN_THUMBR, GamepadButton::RIGHT_STICK),
            (BTN_DPAD_UP, GamepadButton::DPAD_UP),
            (BTN_DPAD_RIGHT, GamepadButton::DPAD_RIGHT),
        ];

        for (code, button) in codes {
            assert_eq!(
                run(&mut pad, RawEvent::Key { code, value: 1 }),
                vec![pressed(button, true)]
            );
        }

        assert_eq!(
            run(
                &mut pad,
                RawEvent::Key {
                    code: BTN_SOUTH,
                    value: 2
                }
            ),
            Vec::new()
        );
        assert_eq!(
            run(
                &mut pad,
                RawEvent::Key {
                    code: 0x2ff,
                    value: 1
                }
            ),
            Vec::new()
        );
    }

    #[test]
    fn sticks_span_minus_one_to_one_with_y_down() {
        let mut pad = translator(0.0);

        let full = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_Y,
                value: 32767,
            },
        );
        let back = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_X,
                value: -32768,
            },
        );

        assert_eq!(
            full,
            vec![GamepadEvent::Axis {
                axis: GamepadAxis::LEFT_Y,
                value: 1.0
            }]
        );
        assert_eq!(
            back,
            vec![GamepadEvent::Axis {
                axis: GamepadAxis::LEFT_X,
                value: -1.0
            }]
        );
    }

    #[test]
    fn triggers_span_zero_to_one() {
        let mut pad = translator(0.0);

        let half = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_Z,
                value: 255,
            },
        );
        let none = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_RZ,
                value: 0,
            },
        );

        assert_eq!(
            half,
            vec![GamepadEvent::Axis {
                axis: GamepadAxis::LEFT_TRIGGER,
                value: 1.0
            }]
        );
        assert_eq!(
            none,
            vec![GamepadEvent::Axis {
                axis: GamepadAxis::RIGHT_TRIGGER,
                value: 0.0
            }]
        );
    }

    #[test]
    fn the_deadzone_swallows_drift_and_rescales_the_rest() {
        let mut pad = translator(0.2);

        let drift = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_X,
                value: 3000,
            },
        );
        let edge = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_X,
                value: 32767,
            },
        );
        let half = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_RX,
                value: -19661,
            },
        );

        assert!(axis_value(&drift).abs() < 1e-6);
        assert!((axis_value(&edge) - 1.0).abs() < 1e-3);
        assert!((axis_value(&half) + 0.5).abs() < 0.02);
    }

    #[test]
    fn unchanged_axis_values_are_not_repeated() {
        let mut pad = translator(0.0);

        run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_X,
                value: 1000,
            },
        );
        let again = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_X,
                value: 1000,
            },
        );
        let drift_inside_deadzone = {
            let mut pad = translator(0.5);
            run(
                &mut pad,
                RawEvent::Abs {
                    code: ABS_X,
                    value: 100,
                },
            );
            run(
                &mut pad,
                RawEvent::Abs {
                    code: ABS_X,
                    value: 200,
                },
            )
        };

        assert_eq!(again, Vec::new());
        assert_eq!(drift_inside_deadzone, Vec::new());
    }

    #[test]
    fn the_hat_becomes_dpad_buttons() {
        let mut pad = translator(0.0);

        let left = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_HAT0X,
                value: -1,
            },
        );
        let right = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_HAT0X,
                value: 1,
            },
        );
        let up = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_HAT0Y,
                value: -1,
            },
        );
        let centre = run(
            &mut pad,
            RawEvent::Abs {
                code: ABS_HAT0X,
                value: 0,
            },
        );

        assert_eq!(left, vec![pressed(GamepadButton::DPAD_LEFT, true)]);
        assert_eq!(
            right,
            vec![
                pressed(GamepadButton::DPAD_LEFT, false),
                pressed(GamepadButton::DPAD_RIGHT, true)
            ]
        );
        assert_eq!(up, vec![pressed(GamepadButton::DPAD_UP, true)]);
        assert_eq!(centre, vec![pressed(GamepadButton::DPAD_RIGHT, false)]);
    }

    #[test]
    fn digital_triggers_become_full_axis_swings_when_no_analog_ones_exist() {
        let digital = PadCaps {
            layout: FaceLayout::Xpad,
            axes: vec![(ABS_X, STICK), (ABS_Y, STICK)],
        };
        let mut pad = PadTranslator::new(digital, 0.0);

        let down = run(
            &mut pad,
            RawEvent::Key {
                code: BTN_TR2,
                value: 1,
            },
        );
        let up = run(
            &mut pad,
            RawEvent::Key {
                code: BTN_TR2,
                value: 0,
            },
        );

        assert_eq!(
            down,
            vec![GamepadEvent::Axis {
                axis: GamepadAxis::RIGHT_TRIGGER,
                value: 1.0
            }]
        );
        assert!(axis_value(&up).abs() < 1e-6);
    }

    #[test]
    fn analog_triggers_make_the_trigger_buttons_inert() {
        let mut pad = translator(0.0);

        assert_eq!(
            run(
                &mut pad,
                RawEvent::Key {
                    code: BTN_TL2,
                    value: 1
                }
            ),
            Vec::new()
        );
    }

    #[test]
    fn an_axis_the_device_does_not_have_is_ignored() {
        let sticks_only = PadCaps {
            layout: FaceLayout::Xpad,
            axes: vec![(ABS_X, STICK)],
        };
        let mut pad = PadTranslator::new(sticks_only, 0.0);

        assert_eq!(
            run(
                &mut pad,
                RawEvent::Abs {
                    code: ABS_RY,
                    value: 100
                }
            ),
            Vec::new()
        );
    }
}
