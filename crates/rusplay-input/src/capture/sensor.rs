use crate::{GamepadButton, GamepadEvent, MAX_PAD_FINGERS, SensorKind, capture::pad::RawEvent};

const ABS_X: u16 = 0x00;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RZ: u16 = 0x05;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const BTN_LEFT: u16 = 0x110;

const STANDARD_GRAVITY: f64 = 9.80665;
const RADIANS_PER_DEGREE: f64 = std::f64::consts::PI / 180.0;
const AXES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionCaps {
    pub accel_units_per_g: Option<u32>,
    pub gyro_units_per_degree_second: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct Channel {
    sensor: SensorKind,
    scale: f64,
    raw: [i32; AXES],
    seen: [bool; AXES],
    dirty: bool,
}

impl Channel {
    fn new(sensor: SensorKind, units_per_unit: Option<u32>, si_per_unit: f64) -> Option<Self> {
        let resolution = units_per_unit.filter(|resolution| *resolution > 0)?;

        Some(Self {
            sensor,
            scale: si_per_unit / f64::from(resolution),
            raw: [0; AXES],
            seen: [false; AXES],
            dirty: false,
        })
    }

    fn set(&mut self, axis: usize, value: i32) {
        self.raw[axis] = value;
        self.seen[axis] = true;
        self.dirty = true;
    }

    #[allow(clippy::cast_possible_truncation)]
    fn flush(&mut self, out: &mut Vec<GamepadEvent>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;

        if self.seen.iter().all(|seen| *seen) {
            let [x, y, z] = self.raw.map(|raw| (f64::from(raw) * self.scale) as f32);
            out.push(GamepadEvent::Sensor {
                sensor: self.sensor,
                x,
                y,
                z,
            });
        }
    }
}

#[derive(Debug, Clone)]
pub struct MotionTranslator {
    accel: Option<Channel>,
    gyro: Option<Channel>,
}

impl MotionTranslator {
    pub fn new(caps: MotionCaps) -> Self {
        Self {
            accel: Channel::new(SensorKind::Accel, caps.accel_units_per_g, STANDARD_GRAVITY),
            gyro: Channel::new(
                SensorKind::Gyro,
                caps.gyro_units_per_degree_second,
                RADIANS_PER_DEGREE,
            ),
        }
    }

    pub fn translate(&mut self, raw: RawEvent, out: &mut Vec<GamepadEvent>) {
        match raw {
            RawEvent::Abs { code, value } => match code {
                ABS_X..=ABS_Z => {
                    if let Some(channel) = &mut self.accel {
                        channel.set(usize::from(code - ABS_X), value);
                    }
                }
                ABS_RX..=ABS_RZ => {
                    if let Some(channel) = &mut self.gyro {
                        channel.set(usize::from(code - ABS_RX), value);
                    }
                }
                _ => {}
            },
            RawEvent::Sync => {
                for channel in [&mut self.gyro, &mut self.accel].into_iter().flatten() {
                    channel.flush(out);
                }
            }
            RawEvent::Key { .. } => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchpadRange {
    pub min: i32,
    pub max: i32,
}

impl TouchpadRange {
    #[allow(clippy::cast_possible_truncation)]
    fn normalise(self, value: i32) -> f32 {
        let span = f64::from(self.max) - f64::from(self.min);
        if span <= 0.0 {
            return 0.0;
        }

        ((f64::from(value) - f64::from(self.min)) / span).clamp(0.0, 1.0) as f32
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Pending {
    #[default]
    Nothing,
    Lift,
    Touch,
    Replace,
}

#[derive(Debug, Clone, Copy, Default)]
struct Contact {
    down: bool,
    moved: bool,
    pending: Pending,
    x: Option<i32>,
    y: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct TouchpadTranslator {
    x: TouchpadRange,
    y: TouchpadRange,
    contacts: [Contact; MAX_PAD_FINGERS],
    current: Option<usize>,
    clicked: bool,
}

impl TouchpadTranslator {
    pub fn new(x: TouchpadRange, y: TouchpadRange) -> Self {
        Self {
            x,
            y,
            contacts: [Contact::default(); MAX_PAD_FINGERS],
            current: Some(0),
            clicked: false,
        }
    }

    pub fn translate(&mut self, raw: RawEvent, out: &mut Vec<GamepadEvent>) {
        match raw {
            RawEvent::Abs {
                code: ABS_MT_SLOT,
                value,
            } => {
                self.current = usize::try_from(value)
                    .ok()
                    .filter(|slot| *slot < MAX_PAD_FINGERS);
            }
            RawEvent::Abs { code, value } => self.contact_event(code, value),
            RawEvent::Key {
                code: BTN_LEFT,
                value,
            } if value <= 1 => {
                let pressed = value == 1;
                if pressed != self.clicked {
                    self.clicked = pressed;
                    out.push(GamepadEvent::Button {
                        button: GamepadButton::TOUCHPAD,
                        pressed,
                    });
                }
            }
            RawEvent::Key { .. } => {}
            RawEvent::Sync => self.flush(out),
        }
    }

    pub fn release(&mut self, out: &mut Vec<GamepadEvent>) {
        for (finger, contact) in (0u8..).zip(&self.contacts) {
            if contact.down {
                out.push(GamepadEvent::TouchpadUp { finger });
            }
        }
        if self.clicked {
            out.push(GamepadEvent::Button {
                button: GamepadButton::TOUCHPAD,
                pressed: false,
            });
        }

        self.contacts = [Contact::default(); MAX_PAD_FINGERS];
        self.clicked = false;
    }

    fn contact_event(&mut self, code: u16, value: i32) {
        let Some(contact) = self.current.map(|slot| &mut self.contacts[slot]) else {
            return;
        };

        match code {
            ABS_MT_TRACKING_ID => {
                contact.pending = match (value < 0, contact.down) {
                    (true, _) => Pending::Lift,
                    (false, true) => Pending::Replace,
                    (false, false) => Pending::Touch,
                };
                contact.x = None;
                contact.y = None;
            }
            ABS_MT_POSITION_X => {
                contact.x = Some(value);
                contact.moved = true;
            }
            ABS_MT_POSITION_Y => {
                contact.y = Some(value);
                contact.moved = true;
            }
            _ => {}
        }
    }

    fn flush(&mut self, out: &mut Vec<GamepadEvent>) {
        for (finger, contact) in (0u8..).zip(&mut self.contacts) {
            if matches!(contact.pending, Pending::Lift | Pending::Replace) {
                if contact.down {
                    out.push(GamepadEvent::TouchpadUp { finger });
                }
                contact.down = false;
                contact.pending = match contact.pending {
                    Pending::Replace => Pending::Touch,
                    _ => Pending::Nothing,
                };
            }

            if let (Some(x), Some(y)) = (contact.x, contact.y) {
                let (x, y) = (self.x.normalise(x), self.y.normalise(y));
                if contact.pending == Pending::Touch {
                    contact.pending = Pending::Nothing;
                    contact.down = true;
                    out.push(GamepadEvent::TouchpadDown { finger, x, y });
                } else if contact.down && contact.moved {
                    out.push(GamepadEvent::TouchpadMotion { finger, x, y });
                }
            }
            contact.moved = false;
        }
    }
}

#[derive(Debug, Clone)]
pub enum SiblingTranslator {
    Motion(MotionTranslator),
    Touchpad(TouchpadTranslator),
}

impl SiblingTranslator {
    pub fn translate(&mut self, raw: RawEvent, out: &mut Vec<GamepadEvent>) {
        match self {
            Self::Motion(motion) => motion.translate(raw, out),
            Self::Touchpad(touchpad) => touchpad.translate(raw, out),
        }
    }

    pub fn release(&mut self, out: &mut Vec<GamepadEvent>) {
        if let Self::Touchpad(touchpad) = self {
            touchpad.release(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAVITY_RESOLUTION: u32 = 8192;
    const GYRO_RESOLUTION: u32 = 1024;
    const PAD: TouchpadRange = TouchpadRange { min: 0, max: 1000 };

    fn abs(code: u16, value: i32) -> RawEvent {
        RawEvent::Abs { code, value }
    }

    fn motion() -> MotionTranslator {
        MotionTranslator::new(MotionCaps {
            accel_units_per_g: Some(GRAVITY_RESOLUTION),
            gyro_units_per_degree_second: Some(GYRO_RESOLUTION),
        })
    }

    fn run(translator: &mut TouchpadTranslator, raws: &[RawEvent]) -> Vec<GamepadEvent> {
        let mut out = Vec::new();
        for raw in raws {
            translator.translate(*raw, &mut out);
        }
        out
    }

    fn touchpad() -> TouchpadTranslator {
        TouchpadTranslator::new(PAD, PAD)
    }

    fn down(finger: u8, x: f32, y: f32) -> GamepadEvent {
        GamepadEvent::TouchpadDown { finger, x, y }
    }

    fn slide(finger: u8, x: f32, y: f32) -> GamepadEvent {
        GamepadEvent::TouchpadMotion { finger, x, y }
    }

    fn lift(finger: u8) -> GamepadEvent {
        GamepadEvent::TouchpadUp { finger }
    }

    #[test]
    fn accelerometer_units_become_metres_per_second_squared_on_sync() {
        let mut translator = motion();
        let mut out = Vec::new();

        translator.translate(abs(0, 0), &mut out);
        translator.translate(abs(1, i32::try_from(GRAVITY_RESOLUTION).unwrap()), &mut out);
        translator.translate(abs(2, -4096), &mut out);

        assert_eq!(out, Vec::new());

        translator.translate(RawEvent::Sync, &mut out);

        assert_eq!(
            out,
            vec![GamepadEvent::Sensor {
                sensor: SensorKind::Accel,
                x: 0.0,
                y: 9.80665,
                z: -4.903_325,
            }]
        );
    }

    #[test]
    fn gyroscope_units_become_radians_per_second() {
        let mut translator = motion();
        let mut out = Vec::new();

        for (code, value) in [(3, 1024), (4, -2048), (5, 0)] {
            translator.translate(abs(code, value), &mut out);
        }
        translator.translate(RawEvent::Sync, &mut out);

        let [GamepadEvent::Sensor { sensor, x, y, z }] = out[..] else {
            panic!("expected one sensor event, got {out:?}");
        };
        assert_eq!(sensor, SensorKind::Gyro);
        assert!((x - 1.0f32.to_radians()).abs() < 1e-6);
        assert!((y + 2.0f32.to_radians()).abs() < 1e-6);
        assert_eq!(z, 0.0);
    }

    #[test]
    fn a_sensor_is_withheld_until_every_axis_has_reported() {
        let mut translator = motion();
        let mut out = Vec::new();

        translator.translate(abs(3, 10), &mut out);
        translator.translate(abs(4, 10), &mut out);
        translator.translate(RawEvent::Sync, &mut out);

        assert_eq!(out, Vec::new());

        translator.translate(abs(5, 10), &mut out);
        translator.translate(RawEvent::Sync, &mut out);

        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_sync_without_new_samples_emits_nothing_and_a_partial_update_resends_the_triple() {
        let mut translator = motion();
        let mut out = Vec::new();

        for code in 0..3 {
            translator.translate(abs(code, 8192), &mut out);
        }
        translator.translate(RawEvent::Sync, &mut out);
        translator.translate(RawEvent::Sync, &mut out);
        translator.translate(abs(1, 0), &mut out);
        translator.translate(RawEvent::Sync, &mut out);

        assert_eq!(out.len(), 2);
        assert!(matches!(
            out[1],
            GamepadEvent::Sensor { x, y, z, .. } if x > 9.0 && y == 0.0 && z > 9.0
        ));
    }

    #[test]
    fn a_sensor_without_a_resolution_is_never_reported() {
        let mut translator = MotionTranslator::new(MotionCaps {
            accel_units_per_g: None,
            gyro_units_per_degree_second: Some(0),
        });
        let mut out = Vec::new();

        for code in 0..6 {
            translator.translate(abs(code, 100), &mut out);
        }
        translator.translate(RawEvent::Sync, &mut out);

        assert_eq!(out, Vec::new());
    }

    #[test]
    fn extreme_raw_values_stay_finite() {
        let mut translator = motion();
        let mut out = Vec::new();

        for code in 0..3 {
            translator.translate(abs(code, i32::MIN), &mut out);
        }
        translator.translate(RawEvent::Sync, &mut out);

        let [GamepadEvent::Sensor { x, y, z, .. }] = out[..] else {
            panic!("expected one sensor event, got {out:?}");
        };
        assert!(x.is_finite() && y.is_finite() && z.is_finite());
    }

    #[test]
    fn a_contact_goes_down_moves_and_lifts_on_sync() {
        let mut translator = touchpad();

        let first = run(
            &mut translator,
            &[
                abs(ABS_MT_SLOT, 0),
                abs(ABS_MT_TRACKING_ID, 7),
                abs(ABS_MT_POSITION_X, 250),
                abs(ABS_MT_POSITION_Y, 500),
                RawEvent::Sync,
            ],
        );
        let moved = run(
            &mut translator,
            &[abs(ABS_MT_POSITION_X, 300), RawEvent::Sync],
        );
        let quiet = run(&mut translator, &[RawEvent::Sync]);
        let up = run(
            &mut translator,
            &[abs(ABS_MT_TRACKING_ID, -1), RawEvent::Sync],
        );

        assert_eq!(first, vec![down(0, 0.25, 0.5)]);
        assert_eq!(moved, vec![slide(0, 0.3, 0.5)]);
        assert_eq!(quiet, Vec::new());
        assert_eq!(up, vec![lift(0)]);
    }

    #[test]
    fn two_slots_are_tracked_independently() {
        let mut translator = touchpad();

        let events = run(
            &mut translator,
            &[
                abs(ABS_MT_SLOT, 0),
                abs(ABS_MT_TRACKING_ID, 1),
                abs(ABS_MT_POSITION_X, 100),
                abs(ABS_MT_POSITION_Y, 100),
                abs(ABS_MT_SLOT, 1),
                abs(ABS_MT_TRACKING_ID, 2),
                abs(ABS_MT_POSITION_X, 900),
                abs(ABS_MT_POSITION_Y, 800),
                RawEvent::Sync,
                abs(ABS_MT_SLOT, 0),
                abs(ABS_MT_TRACKING_ID, -1),
                RawEvent::Sync,
            ],
        );

        assert_eq!(events, vec![down(0, 0.1, 0.1), down(1, 0.9, 0.8), lift(0)]);
    }

    #[test]
    fn a_down_waits_for_both_coordinates() {
        let mut translator = touchpad();

        let early = run(
            &mut translator,
            &[
                abs(ABS_MT_TRACKING_ID, 3),
                abs(ABS_MT_POSITION_X, 500),
                RawEvent::Sync,
            ],
        );
        let complete = run(
            &mut translator,
            &[abs(ABS_MT_POSITION_Y, 1000), RawEvent::Sync],
        );

        assert_eq!(early, Vec::new());
        assert_eq!(complete, vec![down(0, 0.5, 1.0)]);
    }

    #[test]
    fn a_new_tracking_id_in_a_down_slot_lifts_then_touches() {
        let mut translator = touchpad();
        run(
            &mut translator,
            &[
                abs(ABS_MT_TRACKING_ID, 1),
                abs(ABS_MT_POSITION_X, 0),
                abs(ABS_MT_POSITION_Y, 0),
                RawEvent::Sync,
            ],
        );

        let events = run(
            &mut translator,
            &[
                abs(ABS_MT_TRACKING_ID, 2),
                abs(ABS_MT_POSITION_X, 1000),
                abs(ABS_MT_POSITION_Y, 1000),
                RawEvent::Sync,
            ],
        );

        assert_eq!(events, vec![lift(0), down(0, 1.0, 1.0)]);
    }

    #[test]
    fn slots_past_the_finger_limit_and_out_of_range_positions_are_contained() {
        let mut translator = touchpad();

        let ignored = run(
            &mut translator,
            &[
                abs(ABS_MT_SLOT, i32::try_from(MAX_PAD_FINGERS).unwrap()),
                abs(ABS_MT_TRACKING_ID, 1),
                abs(ABS_MT_POSITION_X, 10),
                abs(ABS_MT_POSITION_Y, 10),
                RawEvent::Sync,
                abs(ABS_MT_SLOT, -1),
                abs(ABS_MT_TRACKING_ID, 1),
                RawEvent::Sync,
            ],
        );
        let clamped = run(
            &mut translator,
            &[
                abs(ABS_MT_SLOT, 2),
                abs(ABS_MT_TRACKING_ID, 1),
                abs(ABS_MT_POSITION_X, -50),
                abs(ABS_MT_POSITION_Y, 5000),
                RawEvent::Sync,
            ],
        );

        assert_eq!(ignored, Vec::new());
        assert_eq!(clamped, vec![down(2, 0.0, 1.0)]);
    }

    #[test]
    fn the_click_is_the_touchpad_button() {
        let mut translator = touchpad();
        let click = |pressed| GamepadEvent::Button {
            button: GamepadButton::TOUCHPAD,
            pressed,
        };

        let events = run(
            &mut translator,
            &[
                RawEvent::Key {
                    code: BTN_LEFT,
                    value: 1,
                },
                RawEvent::Key {
                    code: BTN_LEFT,
                    value: 2,
                },
                RawEvent::Key {
                    code: BTN_LEFT,
                    value: 1,
                },
                RawEvent::Key {
                    code: BTN_LEFT,
                    value: 0,
                },
            ],
        );

        assert_eq!(events, vec![click(true), click(false)]);
    }

    #[test]
    fn releasing_lifts_every_finger_and_the_click() {
        let mut translator = touchpad();
        run(
            &mut translator,
            &[
                abs(ABS_MT_TRACKING_ID, 1),
                abs(ABS_MT_POSITION_X, 0),
                abs(ABS_MT_POSITION_Y, 0),
                RawEvent::Key {
                    code: BTN_LEFT,
                    value: 1,
                },
                RawEvent::Sync,
            ],
        );
        let mut out = Vec::new();

        translator.release(&mut out);

        assert_eq!(
            out,
            vec![
                lift(0),
                GamepadEvent::Button {
                    button: GamepadButton::TOUCHPAD,
                    pressed: false
                }
            ]
        );

        out.clear();
        translator.release(&mut out);

        assert_eq!(out, Vec::new());
    }
}
