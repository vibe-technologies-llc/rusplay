use std::time::Duration;

use crate::{
    GamepadAxis, GamepadButton, GamepadEvent, InputError, PadFeedback, RumbleCommand, SensorKind,
    TRIGGER_EFFECT_LEN,
};

pub(super) const VENDOR_SONY: u16 = 0x054c;
pub(super) const PRODUCT_DUALSENSE: u16 = 0x0ce6;
pub(super) const VERSION_DUALSENSE: u32 = 0x0100;
pub(super) const NAME: &str = "Sony Interactive Entertainment DualSense Wireless Controller";

pub(super) const INPUT_REPORT_ID: u8 = 0x01;
pub(super) const OUTPUT_REPORT_ID: u8 = 0x02;
pub(super) const CALIBRATION_REPORT_ID: u8 = 0x05;
pub(super) const PAIRING_REPORT_ID: u8 = 0x09;
pub(super) const FIRMWARE_REPORT_ID: u8 = 0x20;

pub(super) const INPUT_REPORT_LEN: usize = 64;
#[cfg(test)]
pub(super) const OUTPUT_REPORT_LEN: usize = 63;
const OUTPUT_COMMON_LEN: usize = 48;
const CALIBRATION_REPORT_LEN: usize = 41;
const PAIRING_REPORT_LEN: usize = 20;
const FIRMWARE_REPORT_LEN: usize = 64;

const TOUCHPAD_WIDTH: u16 = 1920;
const TOUCHPAD_HEIGHT: u16 = 1080;
pub(super) const TOUCH_POINTS: usize = 2;
const TOUCH_INACTIVE: u8 = 0x80;
const TOUCH_ID_MASK: u8 = 0x7f;

const ACCEL_RAW_PER_G: f32 = 8192.0;
const GYRO_RAW_PER_DEGREE_PER_SECOND: f32 = 16.0;
const STANDARD_GRAVITY: f32 = 9.806_65;
const GYRO_SPEED_HALF_RANGE: i16 = 540;
const GYRO_RAW_HALF_RANGE: i16 = GYRO_SPEED_HALF_RANGE * 16;
const ACCEL_RAW_ONE_G: i16 = 8192;

const SENSOR_UNITS_PER_MICROSECOND: u128 = 3;
const TRIGGER_BUTTON_THRESHOLD: u8 = 32;
const BATTERY_FULL: u8 = 0x2a;
const FIRMWARE_UPDATE_VERSION: u16 = 0x0224;
const FIRMWARE_VERSION: u32 = 0x0100_0224;
const HARDWARE_VERSION: u32 = 0x0000_0a00;

const HAT_NEUTRAL: u8 = 8;

const OFFSET_STICKS: usize = 1;
const OFFSET_TRIGGERS: usize = 5;
const OFFSET_SEQUENCE: usize = 7;
const OFFSET_BUTTONS: usize = 8;
const OFFSET_GYRO: usize = 16;
const OFFSET_ACCEL: usize = 22;
const OFFSET_TIMESTAMP: usize = 28;
const OFFSET_TOUCH: usize = 33;
const OFFSET_STATUS: usize = 53;

const VALID0_COMPATIBLE_VIBRATION: u8 = 0x01;
const VALID2_COMPATIBLE_VIBRATION2: u8 = 0x04;
const OUTPUT_OFFSET_VALID0: usize = 1;
const OUTPUT_OFFSET_MOTOR_WEAK: usize = 3;
const OUTPUT_OFFSET_MOTOR_STRONG: usize = 4;
const OUTPUT_OFFSET_VALID1: usize = 2;
const OUTPUT_OFFSET_RIGHT_TRIGGER: usize = 11;
const OUTPUT_OFFSET_LEFT_TRIGGER: usize = 22;
const OUTPUT_OFFSET_VALID2: usize = 39;
const OUTPUT_OFFSET_PLAYER_LEDS: usize = 44;
const OUTPUT_OFFSET_LIGHTBAR: usize = 45;
const VALID0_RIGHT_TRIGGER: u8 = 0x04;
const VALID0_LEFT_TRIGGER: u8 = 0x08;
const VALID1_LIGHTBAR: u8 = 0x04;
const VALID1_PLAYER_LEDS: u8 = 0x10;
const PLAYER_LED_MASK: u8 = 0x1f;

const BUTTONS0_SQUARE: u8 = 0x10;
const BUTTONS0_CROSS: u8 = 0x20;
const BUTTONS0_CIRCLE: u8 = 0x40;
const BUTTONS0_TRIANGLE: u8 = 0x80;
const BUTTONS1_L1: u8 = 0x01;
const BUTTONS1_R1: u8 = 0x02;
const BUTTONS1_L2: u8 = 0x04;
const BUTTONS1_R2: u8 = 0x08;
const BUTTONS1_CREATE: u8 = 0x10;
const BUTTONS1_OPTIONS: u8 = 0x20;
const BUTTONS1_L3: u8 = 0x40;
const BUTTONS1_R3: u8 = 0x80;
const BUTTONS2_HOME: u8 = 0x01;
const BUTTONS2_TOUCHPAD: u8 = 0x02;

pub(super) const REPORT_DESCRIPTOR: [u8; 134] = [
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x06, 0x81, 0x02, 0x06,
    0x00, 0xff, 0x09, 0x20, 0x95, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x39, 0x15, 0x00, 0x25, 0x07,
    0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00, 0x05,
    0x09, 0x19, 0x01, 0x29, 0x0f, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0f, 0x81, 0x02, 0x06,
    0x00, 0xff, 0x09, 0x21, 0x95, 0x05, 0x81, 0x02, 0x09, 0x22, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75,
    0x08, 0x95, 0x35, 0x81, 0x02, 0x85, 0x02, 0x09, 0x23, 0x95, 0x3e, 0x91, 0x02, 0x85, 0x05, 0x09,
    0x33, 0x95, 0x28, 0xb1, 0x02, 0x85, 0x09, 0x09, 0x24, 0x95, 0x13, 0xb1, 0x02, 0x85, 0x20, 0x09,
    0x26, 0x95, 0x3f, 0xb1, 0x02, 0xc0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rumble {
    pub strong: u8,
    pub weak: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OutputReport {
    pub rumble: Option<Rumble>,
    pub lightbar: Option<[u8; 3]>,
    pub player_leds: Option<u8>,
    pub right_trigger: Option<[u8; TRIGGER_EFFECT_LEN]>,
    pub left_trigger: Option<[u8; TRIGGER_EFFECT_LEN]>,
}

impl OutputReport {
    pub fn feedback(&self, slot: u8, out: &mut Vec<PadFeedback>) {
        let widen = |magnitude: u8| u16::from(magnitude) * 257;

        if let Some(rumble) = self.rumble {
            out.push(PadFeedback::Rumble(RumbleCommand {
                slot,
                strong: widen(rumble.strong),
                weak: widen(rumble.weak),
                delay_ms: 0,
                duration_ms: RumbleCommand::UNTIL_REPLACED,
            }));
        }

        if let Some([red, green, blue]) = self.lightbar {
            out.push(PadFeedback::Lightbar {
                slot,
                red,
                green,
                blue,
            });
        }

        if let Some(mask) = self.player_leds {
            out.push(PadFeedback::PlayerLeds { slot, mask });
        }

        for (right, effect) in [(false, self.left_trigger), (true, self.right_trigger)] {
            if let Some(effect) = effect {
                out.push(PadFeedback::TriggerEffect {
                    slot,
                    right,
                    effect,
                });
            }
        }
    }
}

fn trigger_effect(data: &[u8], offset: usize) -> Option<[u8; TRIGGER_EFFECT_LEN]> {
    data.get(offset..offset + TRIGGER_EFFECT_LEN)?
        .try_into()
        .ok()
}

pub(super) fn parse_output(data: &[u8]) -> Option<OutputReport> {
    if data.first() != Some(&OUTPUT_REPORT_ID) || data.len() < OUTPUT_COMMON_LEN {
        return None;
    }

    let valid0 = data[OUTPUT_OFFSET_VALID0];
    let valid1 = data[OUTPUT_OFFSET_VALID1];
    let compatible = valid0 & VALID0_COMPATIBLE_VIBRATION != 0
        || data[OUTPUT_OFFSET_VALID2] & VALID2_COMPATIBLE_VIBRATION2 != 0;

    Some(OutputReport {
        rumble: compatible.then(|| Rumble {
            strong: data[OUTPUT_OFFSET_MOTOR_STRONG],
            weak: data[OUTPUT_OFFSET_MOTOR_WEAK],
        }),
        lightbar: (valid1 & VALID1_LIGHTBAR != 0).then(|| {
            [
                data[OUTPUT_OFFSET_LIGHTBAR],
                data[OUTPUT_OFFSET_LIGHTBAR + 1],
                data[OUTPUT_OFFSET_LIGHTBAR + 2],
            ]
        }),
        player_leds: (valid1 & VALID1_PLAYER_LEDS != 0)
            .then(|| data[OUTPUT_OFFSET_PLAYER_LEDS] & PLAYER_LED_MASK),
        right_trigger: (valid0 & VALID0_RIGHT_TRIGGER != 0)
            .then(|| trigger_effect(data, OUTPUT_OFFSET_RIGHT_TRIGGER))
            .flatten(),
        left_trigger: (valid0 & VALID0_LEFT_TRIGGER != 0)
            .then(|| trigger_effect(data, OUTPUT_OFFSET_LEFT_TRIGGER))
            .flatten(),
    })
}

pub(super) fn feature_report(id: u8, mac: [u8; 6]) -> Option<Vec<u8>> {
    match id {
        CALIBRATION_REPORT_ID => Some(calibration_report()),
        PAIRING_REPORT_ID => Some(pairing_report(mac)),
        FIRMWARE_REPORT_ID => Some(firmware_report()),
        _ => None,
    }
}

fn calibration_report() -> Vec<u8> {
    calibration_report_for(CALIBRATION_REPORT_ID, CALIBRATION_REPORT_LEN)
}

pub(super) fn calibration_report_for(id: u8, len: usize) -> Vec<u8> {
    let mut report = vec![0; len];
    report[0] = id;

    let gyro_bias = [0i16; 3];
    for (axis, bias) in gyro_bias.into_iter().enumerate() {
        put_i16(&mut report, 1 + axis * 2, bias);
        put_i16(&mut report, 7 + axis * 4, GYRO_RAW_HALF_RANGE);
        put_i16(&mut report, 9 + axis * 4, -GYRO_RAW_HALF_RANGE);
    }

    put_i16(&mut report, 19, GYRO_SPEED_HALF_RANGE);
    put_i16(&mut report, 21, GYRO_SPEED_HALF_RANGE);

    for axis in 0..3 {
        put_i16(&mut report, 23 + axis * 4, ACCEL_RAW_ONE_G);
        put_i16(&mut report, 25 + axis * 4, -ACCEL_RAW_ONE_G);
    }

    report
}

fn pairing_report(mac: [u8; 6]) -> Vec<u8> {
    pairing_report_for(PAIRING_REPORT_ID, PAIRING_REPORT_LEN, mac)
}

pub(super) fn pairing_report_for(id: u8, len: usize, mac: [u8; 6]) -> Vec<u8> {
    let mut report = vec![0; len];
    report[0] = id;
    report[1..7].copy_from_slice(&mac);

    report
}

fn firmware_report() -> Vec<u8> {
    let mut report = vec![0; FIRMWARE_REPORT_LEN];
    report[0] = FIRMWARE_REPORT_ID;
    report[24..28].copy_from_slice(&HARDWARE_VERSION.to_le_bytes());
    report[28..32].copy_from_slice(&FIRMWARE_VERSION.to_le_bytes());
    report[44..46].copy_from_slice(&FIRMWARE_UPDATE_VERSION.to_le_bytes());

    report
}

pub(super) fn put_i16(report: &mut [u8], offset: usize, value: i16) {
    report[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Contact {
    id: u8,
    x: u16,
    y: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PadReportState {
    sticks: [u8; 4],
    triggers: [u8; 2],
    held: u16,
    gyro: [i16; 3],
    accel: [i16; 3],
    contacts: [Option<Contact>; TOUCH_POINTS],
    next_contact_id: u8,
    touchpad_height: u16,
}

impl Default for PadReportState {
    fn default() -> Self {
        Self::with_touchpad_height(TOUCHPAD_HEIGHT)
    }
}

impl PadReportState {
    pub fn with_touchpad_height(touchpad_height: u16) -> Self {
        Self {
            sticks: [stick_byte(0.0); 4],
            triggers: [0; 2],
            held: 0,
            gyro: [0; 3],
            accel: [0; 3],
            contacts: [None; TOUCH_POINTS],
            next_contact_id: 0,
            touchpad_height,
        }
    }

    pub fn sticks(&self) -> [u8; 4] {
        self.sticks
    }

    pub fn triggers(&self) -> [u8; 2] {
        self.triggers
    }

    pub fn gyro(&self) -> [i16; 3] {
        self.gyro
    }

    pub fn accel(&self) -> [i16; 3] {
        self.accel
    }

    pub fn contacts(&self) -> [Option<Contact>; TOUCH_POINTS] {
        self.contacts
    }

    pub fn apply(&mut self, event: GamepadEvent) -> Result<bool, InputError> {
        match event {
            GamepadEvent::Connected | GamepadEvent::Disconnected => Ok(false),
            GamepadEvent::Button { button, pressed } => self.set_button(button, pressed),
            GamepadEvent::Axis { axis, value } => self.set_axis(axis, value),
            GamepadEvent::Sensor { sensor, x, y, z } => self.set_sensor(sensor, [x, y, z]),
            GamepadEvent::TouchpadDown { finger, x, y }
            | GamepadEvent::TouchpadMotion { finger, x, y } => self.touch(finger, x, y),
            GamepadEvent::TouchpadUp { finger } => Ok(self.lift(finger)),
        }
    }

    fn set_button(&mut self, button: GamepadButton, pressed: bool) -> Result<bool, InputError> {
        if button.0 > GamepadButton::TOUCHPAD.0 {
            return Err(InputError::GamepadButtonUnknown(button.0));
        }

        let bit = 1u16 << button.0;
        let before = self.held;
        if pressed {
            self.held |= bit;
        } else {
            self.held &= !bit;
        }

        Ok(self.held != before)
    }

    fn set_axis(&mut self, axis: GamepadAxis, value: f32) -> Result<bool, InputError> {
        if !value.is_finite() {
            return Err(InputError::NonFiniteValue);
        }

        let (target, byte) = match axis {
            GamepadAxis::LEFT_X => (&mut self.sticks[0], stick_byte(value)),
            GamepadAxis::LEFT_Y => (&mut self.sticks[1], stick_byte(value)),
            GamepadAxis::RIGHT_X => (&mut self.sticks[2], stick_byte(value)),
            GamepadAxis::RIGHT_Y => (&mut self.sticks[3], stick_byte(value)),
            GamepadAxis::LEFT_TRIGGER => (&mut self.triggers[0], trigger_byte(value)),
            GamepadAxis::RIGHT_TRIGGER => (&mut self.triggers[1], trigger_byte(value)),
            GamepadAxis(unknown) => return Err(InputError::GamepadAxisUnknown(unknown)),
        };
        let changed = *target != byte;
        *target = byte;

        Ok(changed)
    }

    fn set_sensor(&mut self, sensor: SensorKind, values: [f32; 3]) -> Result<bool, InputError> {
        if values.iter().any(|value| !value.is_finite()) {
            return Err(InputError::NonFiniteValue);
        }

        let (target, raw) = match sensor {
            SensorKind::Gyro => (&mut self.gyro, values.map(gyro_raw)),
            SensorKind::Accel => (&mut self.accel, values.map(accel_raw)),
        };
        *target = raw;

        Ok(true)
    }

    fn touch(&mut self, finger: u8, x: f32, y: f32) -> Result<bool, InputError> {
        if !x.is_finite() || !y.is_finite() {
            return Err(InputError::NonFiniteValue);
        }

        let Some(slot) = self.contacts.get_mut(usize::from(finger)) else {
            return Ok(false);
        };
        let id = if let Some(contact) = slot {
            contact.id
        } else {
            let id = self.next_contact_id;
            self.next_contact_id = (id + 1) & TOUCH_ID_MASK;
            id
        };
        *slot = Some(Contact {
            id,
            x: touch_coordinate(x, TOUCHPAD_WIDTH),
            y: touch_coordinate(y, self.touchpad_height),
        });

        Ok(true)
    }

    fn lift(&mut self, finger: u8) -> bool {
        self.contacts
            .get_mut(usize::from(finger))
            .is_some_and(|slot| slot.take().is_some())
    }

    fn is_held(&self, button: GamepadButton) -> bool {
        self.held & (1 << button.0) != 0
    }

    fn hat(&self) -> u8 {
        let axis = |positive, negative| {
            i8::from(self.is_held(positive)) - i8::from(self.is_held(negative))
        };

        match (
            axis(GamepadButton::DPAD_RIGHT, GamepadButton::DPAD_LEFT),
            axis(GamepadButton::DPAD_DOWN, GamepadButton::DPAD_UP),
        ) {
            (0, -1) => 0,
            (1, -1) => 1,
            (1, 0) => 2,
            (1, 1) => 3,
            (0, 1) => 4,
            (-1, 1) => 5,
            (-1, 0) => 6,
            (-1, -1) => 7,
            _ => HAT_NEUTRAL,
        }
    }

    pub fn button_bytes(&self) -> [u8; 3] {
        let face = [
            (GamepadButton::WEST, BUTTONS0_SQUARE),
            (GamepadButton::SOUTH, BUTTONS0_CROSS),
            (GamepadButton::EAST, BUTTONS0_CIRCLE),
            (GamepadButton::NORTH, BUTTONS0_TRIANGLE),
        ];
        let middle = [
            (GamepadButton::LEFT_SHOULDER, BUTTONS1_L1),
            (GamepadButton::RIGHT_SHOULDER, BUTTONS1_R1),
            (GamepadButton::BACK, BUTTONS1_CREATE),
            (GamepadButton::START, BUTTONS1_OPTIONS),
            (GamepadButton::LEFT_STICK, BUTTONS1_L3),
            (GamepadButton::RIGHT_STICK, BUTTONS1_R3),
        ];
        let system = [
            (GamepadButton::GUIDE, BUTTONS2_HOME),
            (GamepadButton::TOUCHPAD, BUTTONS2_TOUCHPAD),
        ];
        let collect = |table: &[(GamepadButton, u8)]| {
            table
                .iter()
                .filter(|(button, _)| self.is_held(*button))
                .fold(0, |bits, (_, bit)| bits | bit)
        };

        let mut buttons1 = collect(&middle);
        for (trigger, bit) in [(0, BUTTONS1_L2), (1, BUTTONS1_R2)] {
            if self.triggers[trigger] >= TRIGGER_BUTTON_THRESHOLD {
                buttons1 |= bit;
            }
        }

        [self.hat() | collect(&face), buttons1, collect(&system)]
    }

    pub fn input_report(&self, sequence: u8, timestamp: u32) -> [u8; INPUT_REPORT_LEN] {
        let mut report = [0; INPUT_REPORT_LEN];
        report[0] = INPUT_REPORT_ID;
        report[OFFSET_STICKS..OFFSET_STICKS + 4].copy_from_slice(&self.sticks);
        report[OFFSET_TRIGGERS..OFFSET_TRIGGERS + 2].copy_from_slice(&self.triggers);
        report[OFFSET_SEQUENCE] = sequence;
        report[OFFSET_BUTTONS..OFFSET_BUTTONS + 3].copy_from_slice(&self.button_bytes());

        for (index, value) in self.gyro.iter().enumerate() {
            put_i16(&mut report, OFFSET_GYRO + index * 2, *value);
        }
        for (index, value) in self.accel.iter().enumerate() {
            put_i16(&mut report, OFFSET_ACCEL + index * 2, *value);
        }
        report[OFFSET_TIMESTAMP..OFFSET_TIMESTAMP + 4].copy_from_slice(&timestamp.to_le_bytes());

        for (index, contact) in self.contacts.iter().enumerate() {
            let offset = OFFSET_TOUCH + index * 4;
            report[offset..offset + 4].copy_from_slice(&touch_point(*contact));
        }

        report[OFFSET_STATUS] = BATTERY_FULL;

        report
    }
}

pub(super) fn touch_point(contact: Option<Contact>) -> [u8; 4] {
    match contact {
        Some(Contact { id, x, y }) => {
            let [x_low, x_high] = x.to_le_bytes();
            let [y_low, y_high] = y.to_le_bytes();

            [
                id & TOUCH_ID_MASK,
                x_low,
                (x_high & 0x0f) | ((y_low & 0x0f) << 4),
                (y_low >> 4) | ((y_high & 0x0f) << 4),
            ]
        }
        None => [TOUCH_INACTIVE, 0, 0, 0],
    }
}

pub(super) fn sensor_timestamp(elapsed: Duration) -> u32 {
    let units = elapsed.as_micros() * SENSOR_UNITS_PER_MICROSECOND;

    u32::try_from(units % (1 << 32)).unwrap_or(0)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn stick_byte(value: f32) -> u8 {
    ((value.clamp(-1.0, 1.0) + 1.0) * 127.5).round() as u8
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn trigger_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn touch_coordinate(value: f32, extent: u16) -> u16 {
    (value.clamp(0.0, 1.0) * f32::from(extent - 1)).round() as u16
}

#[allow(clippy::cast_possible_truncation)]
fn saturate_i16(value: f32) -> i16 {
    value
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

fn gyro_raw(radians_per_second: f32) -> i16 {
    saturate_i16(radians_per_second.to_degrees() * GYRO_RAW_PER_DEGREE_PER_SECOND)
}

fn accel_raw(metres_per_second_squared: f32) -> i16 {
    saturate_i16(metres_per_second_squared / STANDARD_GRAVITY * ACCEL_RAW_PER_G)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::descriptor_walk::walk;

    fn press(state: &mut PadReportState, button: GamepadButton, pressed: bool) {
        state
            .apply(GamepadEvent::Button { button, pressed })
            .unwrap();
    }

    fn report(state: &PadReportState) -> [u8; INPUT_REPORT_LEN] {
        state.input_report(0, 0)
    }

    fn le16(report: &[u8], offset: usize) -> i16 {
        i16::from_le_bytes([report[offset], report[offset + 1]])
    }

    #[test]
    fn descriptor_is_balanced_and_sizes_every_report_the_driver_uses() {
        let walk = walk(&REPORT_DESCRIPTOR);

        assert_eq!(walk.depth, 0);
        assert_eq!(walk.minimum_depth, 0);
        assert_eq!(
            walk.bits[&('i', INPUT_REPORT_ID)],
            (INPUT_REPORT_LEN - 1) * 8
        );
        assert_eq!(
            walk.bits[&('o', OUTPUT_REPORT_ID)],
            (OUTPUT_REPORT_LEN - 1) * 8
        );
        assert_eq!(
            walk.bits[&('f', CALIBRATION_REPORT_ID)],
            (CALIBRATION_REPORT_LEN - 1) * 8
        );
        assert_eq!(
            walk.bits[&('f', PAIRING_REPORT_ID)],
            (PAIRING_REPORT_LEN - 1) * 8
        );
        assert_eq!(
            walk.bits[&('f', FIRMWARE_REPORT_ID)],
            (FIRMWARE_REPORT_LEN - 1) * 8
        );
        assert_eq!(walk.bits.len(), 5);
    }

    #[test]
    fn a_fresh_report_is_centred_released_and_untouched() {
        let state = PadReportState::default();

        let report = state.input_report(7, 0x0102_0304);

        assert_eq!(report[0], 0x01);
        assert_eq!(&report[1..5], &[128, 128, 128, 128]);
        assert_eq!(&report[5..7], &[0, 0]);
        assert_eq!(report[7], 7);
        assert_eq!(&report[8..11], &[HAT_NEUTRAL, 0, 0]);
        assert_eq!(&report[28..32], &[4, 3, 2, 1]);
        assert_eq!(report[33], 0x80);
        assert_eq!(report[37], 0x80);
        assert_eq!(report[53], BATTERY_FULL);
        assert!(report[11..16].iter().all(|byte| *byte == 0));
        assert!(report[54..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn sticks_span_the_full_byte_range_with_positive_down() {
        let mut state = PadReportState::default();
        let mut set = |axis, value| {
            state.apply(GamepadEvent::Axis { axis, value }).unwrap();
        };

        set(GamepadAxis::LEFT_X, -1.0);
        set(GamepadAxis::LEFT_Y, 1.0);
        set(GamepadAxis::RIGHT_X, 7.0);
        set(GamepadAxis::RIGHT_Y, -7.0);

        assert_eq!(&report(&state)[1..5], &[0, 255, 255, 0]);
    }

    #[test]
    fn triggers_span_zero_to_full_and_raise_the_digital_bits_past_the_threshold() {
        let mut state = PadReportState::default();
        let set = |state: &mut PadReportState, axis, value| {
            state.apply(GamepadEvent::Axis { axis, value }).unwrap();
        };

        set(&mut state, GamepadAxis::LEFT_TRIGGER, 1.0);
        set(&mut state, GamepadAxis::RIGHT_TRIGGER, 0.05);
        let report_one = report(&state);

        set(&mut state, GamepadAxis::RIGHT_TRIGGER, 0.5);
        let report_two = report(&state);

        assert_eq!(&report_one[5..7], &[255, 13]);
        assert_eq!(report_one[9], BUTTONS1_L2);
        assert_eq!(&report_two[5..7], &[255, 128]);
        assert_eq!(report_two[9], BUTTONS1_L2 | BUTTONS1_R2);
    }

    #[test]
    fn dpad_buttons_fold_into_the_eight_way_hat() {
        let mut state = PadReportState::default();
        let hat = |state: &PadReportState| report(state)[8] & 0x0f;
        let cases = [
            (vec![GamepadButton::DPAD_UP], 0),
            (vec![GamepadButton::DPAD_UP, GamepadButton::DPAD_RIGHT], 1),
            (vec![GamepadButton::DPAD_RIGHT], 2),
            (vec![GamepadButton::DPAD_RIGHT, GamepadButton::DPAD_DOWN], 3),
            (vec![GamepadButton::DPAD_DOWN], 4),
            (vec![GamepadButton::DPAD_DOWN, GamepadButton::DPAD_LEFT], 5),
            (vec![GamepadButton::DPAD_LEFT], 6),
            (vec![GamepadButton::DPAD_LEFT, GamepadButton::DPAD_UP], 7),
            (vec![], HAT_NEUTRAL),
            (
                vec![GamepadButton::DPAD_UP, GamepadButton::DPAD_DOWN],
                HAT_NEUTRAL,
            ),
            (
                vec![
                    GamepadButton::DPAD_UP,
                    GamepadButton::DPAD_DOWN,
                    GamepadButton::DPAD_LEFT,
                ],
                6,
            ),
        ];

        for (held, expected) in cases {
            for button in &held {
                press(&mut state, *button, true);
            }

            assert_eq!(hat(&state), expected, "{held:?}");

            for button in held {
                press(&mut state, button, false);
            }
        }
    }

    #[test]
    fn face_shoulder_and_system_buttons_land_on_the_documented_bits() {
        let mut state = PadReportState::default();
        let cases = [
            (GamepadButton::WEST, 8, 0x10),
            (GamepadButton::SOUTH, 8, 0x20),
            (GamepadButton::EAST, 8, 0x40),
            (GamepadButton::NORTH, 8, 0x80),
            (GamepadButton::LEFT_SHOULDER, 9, 0x01),
            (GamepadButton::RIGHT_SHOULDER, 9, 0x02),
            (GamepadButton::BACK, 9, 0x10),
            (GamepadButton::START, 9, 0x20),
            (GamepadButton::LEFT_STICK, 9, 0x40),
            (GamepadButton::RIGHT_STICK, 9, 0x80),
            (GamepadButton::GUIDE, 10, 0x01),
            (GamepadButton::TOUCHPAD, 10, 0x02),
        ];

        for (button, byte, bit) in cases {
            press(&mut state, button, true);
            let held = report(&state);
            press(&mut state, button, false);
            let released = report(&state);

            let hat = if byte == 8 { HAT_NEUTRAL } else { 0 };
            assert_eq!(held[byte], hat | bit, "{button:?}");
            assert_eq!(released[byte], hat, "{button:?}");
        }
    }

    #[test]
    fn unknown_buttons_and_axes_and_non_finite_values_are_errors() {
        let mut state = PadReportState::default();

        let button = state.apply(GamepadEvent::Button {
            button: GamepadButton(16),
            pressed: true,
        });
        let axis = state.apply(GamepadEvent::Axis {
            axis: GamepadAxis(9),
            value: 0.0,
        });
        let nan = state.apply(GamepadEvent::Axis {
            axis: GamepadAxis::LEFT_X,
            value: f32::NAN,
        });
        let sensor = state.apply(GamepadEvent::Sensor {
            sensor: SensorKind::Gyro,
            x: 0.0,
            y: f32::INFINITY,
            z: 0.0,
        });
        let touch = state.apply(GamepadEvent::TouchpadDown {
            finger: 0,
            x: f32::NAN,
            y: 0.0,
        });

        assert!(matches!(button, Err(InputError::GamepadButtonUnknown(16))));
        assert!(matches!(axis, Err(InputError::GamepadAxisUnknown(9))));
        assert!(matches!(nan, Err(InputError::NonFiniteValue)));
        assert!(matches!(sensor, Err(InputError::NonFiniteValue)));
        assert!(matches!(touch, Err(InputError::NonFiniteValue)));
    }

    #[test]
    fn unchanged_buttons_and_axes_ask_for_no_report() {
        let mut state = PadReportState::default();
        let press_south = GamepadEvent::Button {
            button: GamepadButton::SOUTH,
            pressed: true,
        };
        let centre = GamepadEvent::Axis {
            axis: GamepadAxis::LEFT_X,
            value: 0.0,
        };

        assert!(state.apply(press_south).unwrap());
        assert!(!state.apply(press_south).unwrap());
        assert!(!state.apply(centre).unwrap());
        assert!(!state.apply(GamepadEvent::Connected).unwrap());
    }

    #[test]
    fn accelerometer_scales_to_8192_counts_per_g() {
        let mut state = PadReportState::default();

        state
            .apply(GamepadEvent::Sensor {
                sensor: SensorKind::Accel,
                x: STANDARD_GRAVITY,
                y: -STANDARD_GRAVITY / 2.0,
                z: 100.0,
            })
            .unwrap();
        let report = report(&state);

        assert_eq!(le16(&report, 22), 8192);
        assert_eq!(le16(&report, 24), -4096);
        assert_eq!(le16(&report, 26), i16::MAX);
    }

    #[test]
    fn gyroscope_scales_to_16_counts_per_degree_per_second() {
        let mut state = PadReportState::default();

        state
            .apply(GamepadEvent::Sensor {
                sensor: SensorKind::Gyro,
                x: 90.0_f32.to_radians(),
                y: -45.0_f32.to_radians(),
                z: -1000.0,
            })
            .unwrap();
        let report = report(&state);

        assert_eq!(le16(&report, 16), 1440);
        assert_eq!(le16(&report, 18), -720);
        assert_eq!(le16(&report, 20), i16::MIN);
    }

    #[test]
    fn calibrated_sensor_values_equal_the_raw_counts_the_driver_divides_out() {
        let report = calibration_report();
        let field = |offset| i32::from(le16(&report, offset));

        for axis in 0..3 {
            let bias = field(1 + axis * 2);
            let plus = field(7 + axis * 4);
            let minus = field(9 + axis * 4);
            let speed_numerator = (field(19) + field(21)) * 1024;
            let denominator = (plus - bias).abs() + (minus - bias).abs();

            assert_ne!(denominator, 0);
            assert_eq!(speed_numerator * 16 / denominator, 1024);
        }

        for axis in 0..3 {
            let plus = field(23 + axis * 4);
            let minus = field(25 + axis * 4);
            let range = plus - minus;
            let bias = plus - range / 2;

            assert_ne!(range, 0);
            assert_eq!(bias, 0);
            assert_eq!(2 * 8192, range);
        }
    }

    #[test]
    fn touch_contacts_encode_twelve_bit_coordinates_with_an_inactive_flag() {
        let mut state = PadReportState::default();

        state
            .apply(GamepadEvent::TouchpadDown {
                finger: 0,
                x: 1.0,
                y: 1.0,
            })
            .unwrap();
        state
            .apply(GamepadEvent::TouchpadDown {
                finger: 1,
                x: 0.5,
                y: 0.25,
            })
            .unwrap();
        let both = report(&state);

        assert_eq!(&both[33..37], &[0, 0x7f, 0x77, 0x43]);
        assert_eq!(both[37], 1);
        let x = u16::from(both[38]) | (u16::from(both[39] & 0x0f) << 8);
        let y = u16::from(both[39] >> 4) | (u16::from(both[40]) << 4);
        assert_eq!((x, y), (960, 270));
    }

    #[test]
    fn a_new_touch_takes_the_next_contact_id_and_a_lift_marks_the_point_inactive() {
        let mut state = PadReportState::default();
        let down = GamepadEvent::TouchpadDown {
            finger: 0,
            x: 0.0,
            y: 0.0,
        };
        let motion = GamepadEvent::TouchpadMotion {
            finger: 0,
            x: 0.1,
            y: 0.1,
        };

        state.apply(down).unwrap();
        state.apply(motion).unwrap();
        let held = report(&state);
        let lifted = state.apply(GamepadEvent::TouchpadUp { finger: 0 }).unwrap();
        let second_lift = state.apply(GamepadEvent::TouchpadUp { finger: 0 }).unwrap();
        state.apply(down).unwrap();
        let again = report(&state);

        assert_eq!(held[33], 0);
        assert!(lifted);
        assert!(!second_lift);
        assert_eq!(again[33], 1);
    }

    #[test]
    fn fingers_beyond_the_two_contact_points_are_not_shown() {
        let mut state = PadReportState::default();

        let shown = state
            .apply(GamepadEvent::TouchpadDown {
                finger: 2,
                x: 0.5,
                y: 0.5,
            })
            .unwrap();
        let lifted = state.apply(GamepadEvent::TouchpadUp { finger: 3 }).unwrap();

        assert!(!shown);
        assert!(!lifted);
        assert_eq!(report(&state)[33], 0x80);
    }

    #[test]
    fn contact_id_wraps_inside_seven_bits() {
        let mut state = PadReportState {
            next_contact_id: 127,
            ..PadReportState::default()
        };
        let down = GamepadEvent::TouchpadDown {
            finger: 0,
            x: 0.0,
            y: 0.0,
        };

        state.apply(down).unwrap();
        let first = report(&state)[33];
        state.apply(GamepadEvent::TouchpadUp { finger: 0 }).unwrap();
        state.apply(down).unwrap();
        let second = report(&state)[33];

        assert_eq!((first, second), (127, 0));
    }

    #[test]
    fn sensor_timestamps_run_at_three_units_per_microsecond_and_wrap() {
        assert_eq!(sensor_timestamp(Duration::from_micros(10)), 30);
        assert_eq!(
            sensor_timestamp(Duration::from_micros((1 << 32) / 3 + 1)),
            2
        );
    }

    #[test]
    fn pairing_report_carries_the_mac_after_the_id() {
        let mac = [2, 3, 4, 5, 6, 7];

        let report = feature_report(PAIRING_REPORT_ID, mac).unwrap();

        assert_eq!(report.len(), 20);
        assert_eq!(&report[..7], &[0x09, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn firmware_report_carries_the_versions_the_driver_reads() {
        let report = feature_report(FIRMWARE_REPORT_ID, [0; 6]).unwrap();

        assert_eq!(report.len(), 64);
        assert_eq!(report[0], 0x20);
        assert_eq!(&report[24..28], &HARDWARE_VERSION.to_le_bytes());
        assert_eq!(&report[28..32], &FIRMWARE_VERSION.to_le_bytes());
        assert_eq!(&report[44..46], &FIRMWARE_UPDATE_VERSION.to_le_bytes());
    }

    #[test]
    fn calibration_report_is_41_bytes_with_the_id_first() {
        let report = feature_report(CALIBRATION_REPORT_ID, [0; 6]).unwrap();

        assert_eq!(report.len(), 41);
        assert_eq!(report[0], 0x05);
        assert_eq!(le16(&report, 7), 8640);
        assert_eq!(le16(&report, 9), -8640);
        assert_eq!(le16(&report, 19), 540);
        assert_eq!(le16(&report, 21), 540);
        assert_eq!(le16(&report, 23), 8192);
        assert_eq!(le16(&report, 25), -8192);
    }

    #[test]
    fn unknown_feature_reports_have_no_answer() {
        assert!(feature_report(0x81, [0; 6]).is_none());
    }

    fn output(valid0: u8, valid2: u8, strong: u8, weak: u8) -> Vec<u8> {
        let mut data = vec![0; OUTPUT_REPORT_LEN];
        data[0] = OUTPUT_REPORT_ID;
        data[1] = valid0;
        data[39] = valid2;
        data[3] = weak;
        data[4] = strong;

        data
    }

    #[test]
    fn output_report_rumble_comes_from_either_vibration_flag() {
        let compatible = parse_output(&output(0x03, 0, 200, 100)).unwrap();
        let second = parse_output(&output(0x02, 0x04, 7, 9)).unwrap();

        assert_eq!(
            compatible.rumble,
            Some(Rumble {
                strong: 200,
                weak: 100
            })
        );
        assert_eq!(second.rumble, Some(Rumble { strong: 7, weak: 9 }));
    }

    #[test]
    fn output_report_without_any_flag_carries_nothing() {
        let parsed = parse_output(&output(0x02, 0x02, 200, 100)).unwrap();

        assert_eq!(
            parsed,
            OutputReport {
                rumble: None,
                lightbar: None,
                player_leds: None,
                right_trigger: None,
                left_trigger: None
            }
        );
    }

    #[test]
    fn short_or_foreign_output_reports_are_ignored() {
        let mut other = output(0x03, 0, 1, 1);
        other[0] = 0x31;

        assert!(parse_output(&output(0x03, 0, 1, 1)[..47]).is_none());
        assert!(parse_output(&other).is_none());
        assert!(parse_output(&[]).is_none());
    }

    #[test]
    fn lightbar_and_player_leds_are_read_when_their_flags_are_set() {
        let mut data = output(0, 0, 0, 0);
        data[2] = 0x04 | 0x10;
        data[44] = 0xff;
        data[45..48].copy_from_slice(&[10, 20, 30]);

        let parsed = parse_output(&data).unwrap();

        assert_eq!(parsed.lightbar, Some([10, 20, 30]));
        assert_eq!(parsed.player_leds, Some(0x1f));

        data[2] = 0x10;
        let leds_only = parse_output(&data).unwrap();
        assert_eq!(leds_only.lightbar, None);
        data[2] = 0x04;
        let bar_only = parse_output(&data).unwrap();
        assert_eq!(bar_only.player_leds, None);
    }

    #[test]
    fn trigger_effects_are_read_from_their_own_offsets_and_flags() {
        let mut data = output(0x04, 0, 0, 0);
        data[11..22].copy_from_slice(&[1; 11]);
        data[22..33].copy_from_slice(&[2; 11]);

        let right_only = parse_output(&data).unwrap();
        data[1] = 0x08;
        let left_only = parse_output(&data).unwrap();

        assert_eq!(right_only.right_trigger, Some([1; 11]));
        assert_eq!(right_only.left_trigger, None);
        assert_eq!(left_only.left_trigger, Some([2; 11]));
        assert_eq!(left_only.right_trigger, None);
    }

    #[test]
    fn feedback_lists_every_present_field_for_the_slot() {
        let mut data = output(0x01 | 0x04 | 0x08, 0, 255, 128);
        data[2] = 0x04 | 0x10;
        data[44] = 3;
        data[45..48].copy_from_slice(&[1, 2, 3]);
        data[11] = 9;
        data[22] = 8;
        let mut out = Vec::new();

        parse_output(&data).unwrap().feedback(6, &mut out);

        assert_eq!(
            out,
            vec![
                PadFeedback::Rumble(RumbleCommand {
                    slot: 6,
                    strong: 0xffff,
                    weak: 0x8080,
                    delay_ms: 0,
                    duration_ms: 0
                }),
                PadFeedback::Lightbar {
                    slot: 6,
                    red: 1,
                    green: 2,
                    blue: 3
                },
                PadFeedback::PlayerLeds { slot: 6, mask: 3 },
                PadFeedback::TriggerEffect {
                    slot: 6,
                    right: false,
                    effect: [8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
                },
                PadFeedback::TriggerEffect {
                    slot: 6,
                    right: true,
                    effect: [9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
                },
            ]
        );
    }
}
