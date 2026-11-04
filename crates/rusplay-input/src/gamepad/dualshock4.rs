use std::time::Duration;

use crate::gamepad::dualsense::{self, OutputReport, PadReportState, Rumble, put_i16, touch_point};

pub(super) const PRODUCT_DUALSHOCK4: u16 = 0x09cc;
pub(super) const VERSION_DUALSHOCK4: u32 = 0x0100;
pub(super) const NAME: &str = "Sony Interactive Entertainment Wireless Controller";

pub(super) const INPUT_REPORT_ID: u8 = 0x01;
pub(super) const OUTPUT_REPORT_ID: u8 = 0x05;
pub(super) const CALIBRATION_REPORT_ID: u8 = 0x02;
pub(super) const PAIRING_REPORT_ID: u8 = 0x12;
pub(super) const FIRMWARE_REPORT_ID: u8 = 0xa3;

pub(super) const INPUT_REPORT_LEN: usize = 64;
#[cfg(test)]
pub(super) const OUTPUT_REPORT_LEN: usize = 32;
const OUTPUT_COMMON_LEN: usize = 11;
const CALIBRATION_REPORT_LEN: usize = 37;
const PAIRING_REPORT_LEN: usize = 16;
const FIRMWARE_REPORT_LEN: usize = 49;

pub(super) const TOUCHPAD_HEIGHT: u16 = 942;

const SENSOR_MICROSECONDS_PER_16_UNITS: u128 = 3;
const SENSOR_UNIT_DENOMINATOR: u128 = 16;
const COUNTER_MASK: u8 = 0x3f;
const COUNTER_SHIFT: u8 = 2;
const STATUS_CABLE_AND_FULL: u8 = 0x1b;
const HARDWARE_VERSION: u16 = 0x0100;
const FIRMWARE_VERSION: u16 = 0x0100;

const OFFSET_STICKS: usize = 1;
const OFFSET_BUTTONS: usize = 5;
const OFFSET_TRIGGERS: usize = 8;
const OFFSET_TIMESTAMP: usize = 10;
const OFFSET_GYRO: usize = 13;
const OFFSET_ACCEL: usize = 19;
const OFFSET_STATUS: usize = 30;
const OFFSET_TOUCH_COUNT: usize = 33;
const OFFSET_TOUCH_REPORT: usize = 34;
const OFFSET_FIRMWARE_HARDWARE: usize = 35;
const OFFSET_FIRMWARE_VERSION: usize = 41;

const OUTPUT_OFFSET_VALID0: usize = 1;
const OUTPUT_OFFSET_MOTOR_WEAK: usize = 4;
const OUTPUT_OFFSET_MOTOR_STRONG: usize = 5;
const OUTPUT_OFFSET_LIGHTBAR: usize = 6;
const VALID0_MOTOR: u8 = 0x01;
const VALID0_LIGHTBAR: u8 = 0x02;

pub(super) const REPORT_DESCRIPTOR: [u8; 138] = [
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25,
    0x07, 0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00,
    0x05, 0x09, 0x19, 0x01, 0x29, 0x0e, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0e, 0x81, 0x02,
    0x06, 0x00, 0xff, 0x09, 0x20, 0x75, 0x06, 0x95, 0x01, 0x15, 0x00, 0x25, 0x7f, 0x81, 0x02, 0x05,
    0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
    0x06, 0x00, 0xff, 0x09, 0x21, 0x95, 0x36, 0x81, 0x02, 0x85, 0x05, 0x09, 0x22, 0x95, 0x1f, 0x91,
    0x02, 0x85, 0x02, 0x09, 0x24, 0x95, 0x24, 0xb1, 0x02, 0x85, 0x12, 0x09, 0x25, 0x95, 0x0f, 0xb1,
    0x02, 0x85, 0xa3, 0x09, 0x26, 0x95, 0x30, 0xb1, 0x02, 0xc0,
];

pub(super) fn sensor_timestamp(elapsed: Duration) -> u16 {
    let units = elapsed.as_micros() * SENSOR_MICROSECONDS_PER_16_UNITS / SENSOR_UNIT_DENOMINATOR;

    u16::try_from(units % (1 << 16)).unwrap_or(0)
}

pub(super) fn input_report(
    state: &PadReportState,
    sequence: u8,
    timestamp: u16,
) -> [u8; INPUT_REPORT_LEN] {
    let mut report = [0; INPUT_REPORT_LEN];
    report[0] = INPUT_REPORT_ID;
    report[OFFSET_STICKS..OFFSET_STICKS + 4].copy_from_slice(&state.sticks());

    let [face, shoulders, system] = state.button_bytes();
    let counter = (sequence & COUNTER_MASK) << COUNTER_SHIFT;
    report[OFFSET_BUTTONS..OFFSET_BUTTONS + 3].copy_from_slice(&[
        face,
        shoulders,
        system | counter,
    ]);
    report[OFFSET_TRIGGERS..OFFSET_TRIGGERS + 2].copy_from_slice(&state.triggers());
    report[OFFSET_TIMESTAMP..OFFSET_TIMESTAMP + 2].copy_from_slice(&timestamp.to_le_bytes());

    for (index, value) in state.gyro().iter().enumerate() {
        put_i16(&mut report, OFFSET_GYRO + index * 2, *value);
    }
    for (index, value) in state.accel().iter().enumerate() {
        put_i16(&mut report, OFFSET_ACCEL + index * 2, *value);
    }

    report[OFFSET_STATUS] = STATUS_CABLE_AND_FULL;
    report[OFFSET_TOUCH_COUNT] = 1;
    report[OFFSET_TOUCH_REPORT] = sequence;

    let contacts = state.contacts();
    for (index, contact) in contacts.into_iter().enumerate() {
        let offset = OFFSET_TOUCH_REPORT + 1 + index * 4;
        report[offset..offset + 4].copy_from_slice(&touch_point(contact));
    }

    report
}

pub(super) fn parse_output(data: &[u8]) -> Option<OutputReport> {
    if data.first() != Some(&OUTPUT_REPORT_ID) || data.len() < OUTPUT_COMMON_LEN {
        return None;
    }

    let valid0 = data[OUTPUT_OFFSET_VALID0];

    Some(OutputReport {
        rumble: (valid0 & VALID0_MOTOR != 0).then(|| Rumble {
            strong: data[OUTPUT_OFFSET_MOTOR_STRONG],
            weak: data[OUTPUT_OFFSET_MOTOR_WEAK],
        }),
        lightbar: (valid0 & VALID0_LIGHTBAR != 0).then(|| {
            [
                data[OUTPUT_OFFSET_LIGHTBAR],
                data[OUTPUT_OFFSET_LIGHTBAR + 1],
                data[OUTPUT_OFFSET_LIGHTBAR + 2],
            ]
        }),
        player_leds: None,
        right_trigger: None,
        left_trigger: None,
    })
}

pub(super) fn feature_report(id: u8, mac: [u8; 6]) -> Option<Vec<u8>> {
    match id {
        CALIBRATION_REPORT_ID => Some(dualsense::calibration_report_for(
            CALIBRATION_REPORT_ID,
            CALIBRATION_REPORT_LEN,
        )),
        PAIRING_REPORT_ID => Some(dualsense::pairing_report_for(
            PAIRING_REPORT_ID,
            PAIRING_REPORT_LEN,
            mac,
        )),
        FIRMWARE_REPORT_ID => Some(firmware_report()),
        _ => None,
    }
}

fn firmware_report() -> Vec<u8> {
    let mut report = vec![0; FIRMWARE_REPORT_LEN];
    report[0] = FIRMWARE_REPORT_ID;
    report[OFFSET_FIRMWARE_HARDWARE..OFFSET_FIRMWARE_HARDWARE + 2]
        .copy_from_slice(&HARDWARE_VERSION.to_le_bytes());
    report[OFFSET_FIRMWARE_VERSION..OFFSET_FIRMWARE_VERSION + 2]
        .copy_from_slice(&FIRMWARE_VERSION.to_le_bytes());

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        GamepadAxis, GamepadButton, GamepadEvent, PadFeedback, RumbleCommand, SensorKind,
        gamepad::descriptor_walk::walk,
    };

    fn state() -> PadReportState {
        PadReportState::with_touchpad_height(TOUCHPAD_HEIGHT)
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
        let report = input_report(&state(), 0, 0x0102);

        assert_eq!(report[0], 0x01);
        assert_eq!(&report[1..5], &[128, 128, 128, 128]);
        assert_eq!(&report[5..8], &[8, 0, 0]);
        assert_eq!(&report[8..10], &[0, 0]);
        assert_eq!(&report[10..12], &[2, 1]);
        assert_eq!(report[12], 0);
        assert_eq!(report[30], 0x1b);
        assert_eq!(report[33], 1);
        assert_eq!(report[35], 0x80);
        assert_eq!(report[39], 0x80);
        assert!(report[13..25].iter().all(|byte| *byte == 0));
        assert!(report[43..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn triggers_sit_after_the_buttons_and_raise_the_digital_bits() {
        let mut state = state();
        for (axis, value) in [
            (GamepadAxis::LEFT_TRIGGER, 1.0),
            (GamepadAxis::RIGHT_TRIGGER, 0.5),
        ] {
            state.apply(GamepadEvent::Axis { axis, value }).unwrap();
        }

        let report = input_report(&state, 0, 0);

        assert_eq!(&report[8..10], &[255, 128]);
        assert_eq!(report[6], 0x04 | 0x08);
    }

    #[test]
    fn buttons_share_the_dualsense_bit_layout() {
        let mut state = state();
        for button in [
            GamepadButton::DPAD_RIGHT,
            GamepadButton::SOUTH,
            GamepadButton::BACK,
            GamepadButton::RIGHT_STICK,
            GamepadButton::GUIDE,
            GamepadButton::TOUCHPAD,
        ] {
            state
                .apply(GamepadEvent::Button {
                    button,
                    pressed: true,
                })
                .unwrap();
        }

        let report = input_report(&state, 0, 0);

        assert_eq!(report[5], 2 | 0x20);
        assert_eq!(report[6], 0x10 | 0x80);
        assert_eq!(report[7], 0x03);
    }

    #[test]
    fn the_report_counter_takes_the_top_six_bits_of_the_last_button_byte() {
        let mut state = state();
        state
            .apply(GamepadEvent::Button {
                button: GamepadButton::GUIDE,
                pressed: true,
            })
            .unwrap();

        let first = input_report(&state, 1, 0);
        let wrapped = input_report(&state, 65, 0);
        let last = input_report(&state, 63, 0);

        assert_eq!(first[7], 0x01 | (1 << 2));
        assert_eq!(wrapped[7], first[7]);
        assert_eq!(last[7], 0x01 | (63 << 2));
        assert_eq!(last[34], 63);
    }

    #[test]
    fn sensors_land_at_their_offsets_with_the_dualsense_scaling() {
        let mut state = state();
        state
            .apply(GamepadEvent::Sensor {
                sensor: SensorKind::Gyro,
                x: 90.0_f32.to_radians(),
                y: -45.0_f32.to_radians(),
                z: 0.0,
            })
            .unwrap();
        state
            .apply(GamepadEvent::Sensor {
                sensor: SensorKind::Accel,
                x: 9.806_65,
                y: 0.0,
                z: -9.806_65 / 2.0,
            })
            .unwrap();

        let report = input_report(&state, 0, 0);

        assert_eq!(le16(&report, 13), 1440);
        assert_eq!(le16(&report, 15), -720);
        assert_eq!(le16(&report, 17), 0);
        assert_eq!(le16(&report, 19), 8192);
        assert_eq!(le16(&report, 23), -4096);
    }

    #[test]
    fn touch_contacts_use_the_dualshock_four_pad_height() {
        let mut state = state();
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
                y: 0.5,
            })
            .unwrap();

        let report = input_report(&state, 9, 0);

        let point = |offset: usize| {
            let x = u16::from(report[offset + 1]) | (u16::from(report[offset + 2] & 0x0f) << 8);
            let y = u16::from(report[offset + 2] >> 4) | (u16::from(report[offset + 3]) << 4);
            (report[offset], x, y)
        };
        assert_eq!(report[33], 1);
        assert_eq!(report[34], 9);
        assert_eq!(point(35), (0, 1919, 941));
        assert_eq!(point(39), (1, 960, 471));
    }

    #[test]
    fn sensor_timestamps_run_at_three_units_per_sixteen_microseconds_and_wrap() {
        assert_eq!(sensor_timestamp(Duration::from_micros(16)), 3);
        assert_eq!(sensor_timestamp(Duration::from_micros(10)), 1);
        assert_eq!(
            sensor_timestamp(Duration::from_micros((1 << 16) * 16 / 3 + 6)),
            1
        );
    }

    fn output(valid0: u8, strong: u8, weak: u8, colour: [u8; 3]) -> Vec<u8> {
        let mut data = vec![0; OUTPUT_REPORT_LEN];
        data[0] = OUTPUT_REPORT_ID;
        data[1] = valid0;
        data[4] = weak;
        data[5] = strong;
        data[6..9].copy_from_slice(&colour);

        data
    }

    #[test]
    fn output_reports_carry_rumble_and_lightbar_only_when_flagged() {
        let both = parse_output(&output(0x03, 200, 100, [1, 2, 3])).unwrap();
        let motors = parse_output(&output(0x01, 7, 9, [1, 2, 3])).unwrap();
        let neither = parse_output(&output(0x04, 7, 9, [1, 2, 3])).unwrap();

        assert_eq!(
            both.rumble,
            Some(Rumble {
                strong: 200,
                weak: 100
            })
        );
        assert_eq!(both.lightbar, Some([1, 2, 3]));
        assert_eq!(motors.lightbar, None);
        assert_eq!(neither.rumble, None);
        assert_eq!(neither.lightbar, None);
        assert_eq!(both.player_leds, None);
        assert_eq!(both.left_trigger, None);
        assert_eq!(both.right_trigger, None);
    }

    #[test]
    fn output_reports_become_feedback_with_the_left_motor_strong() {
        let mut out = Vec::new();

        parse_output(&output(0x03, 255, 128, [0, 0, 9]))
            .unwrap()
            .feedback(4, &mut out);

        assert_eq!(
            out,
            vec![
                PadFeedback::Rumble(RumbleCommand {
                    slot: 4,
                    strong: 0xffff,
                    weak: 0x8080,
                    delay_ms: 0,
                    duration_ms: 0
                }),
                PadFeedback::Lightbar {
                    slot: 4,
                    red: 0,
                    green: 0,
                    blue: 9
                },
            ]
        );
    }

    #[test]
    fn short_or_foreign_output_reports_are_ignored() {
        let mut other = output(0x03, 1, 1, [0; 3]);
        other[0] = 0x02;

        assert!(parse_output(&output(0x03, 1, 1, [0; 3])[..10]).is_none());
        assert!(parse_output(&other).is_none());
        assert!(parse_output(&[]).is_none());
    }

    #[test]
    fn feature_reports_have_the_sizes_the_driver_requests() {
        let mac = [2, 3, 4, 5, 6, 7];

        let calibration = feature_report(CALIBRATION_REPORT_ID, mac).unwrap();
        let pairing = feature_report(PAIRING_REPORT_ID, mac).unwrap();
        let firmware = feature_report(FIRMWARE_REPORT_ID, mac).unwrap();

        assert_eq!(calibration.len(), 37);
        assert_eq!(calibration[0], 0x02);
        assert_eq!(le16(&calibration, 7), 8640);
        assert_eq!(le16(&calibration, 9), -8640);
        assert_eq!(le16(&calibration, 19), 540);
        assert_eq!(le16(&calibration, 23), 8192);
        assert_eq!(le16(&calibration, 33), -8192);
        assert_eq!(pairing.len(), 16);
        assert_eq!(&pairing[..7], &[0x12, 2, 3, 4, 5, 6, 7]);
        assert_eq!(firmware.len(), 49);
        assert_eq!(firmware[0], 0xa3);
        assert_eq!(le16(&firmware, 35), 0x0100);
        assert_eq!(le16(&firmware, 41), 0x0100);
    }

    #[test]
    fn unknown_feature_reports_have_no_answer() {
        assert!(feature_report(0x81, [0; 6]).is_none());
        assert!(feature_report(dualsense::CALIBRATION_REPORT_ID, [0; 6]).is_none());
    }
}
