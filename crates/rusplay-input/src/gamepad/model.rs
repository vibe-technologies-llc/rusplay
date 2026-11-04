use std::time::Duration;

use crate::gamepad::{
    dualsense::{self, OutputReport, PadReportState},
    dualshock4,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PadModel {
    DualSense,
    DualShock4,
}

impl PadModel {
    pub const fn name(self) -> &'static str {
        match self {
            Self::DualSense => dualsense::NAME,
            Self::DualShock4 => dualshock4::NAME,
        }
    }

    pub const fn product(self) -> u16 {
        match self {
            Self::DualSense => dualsense::PRODUCT_DUALSENSE,
            Self::DualShock4 => dualshock4::PRODUCT_DUALSHOCK4,
        }
    }

    pub const fn version(self) -> u32 {
        match self {
            Self::DualSense => dualsense::VERSION_DUALSENSE,
            Self::DualShock4 => dualshock4::VERSION_DUALSHOCK4,
        }
    }

    pub const fn descriptor(self) -> &'static [u8] {
        match self {
            Self::DualSense => &dualsense::REPORT_DESCRIPTOR,
            Self::DualShock4 => &dualshock4::REPORT_DESCRIPTOR,
        }
    }

    pub fn new_state(self) -> PadReportState {
        match self {
            Self::DualSense => PadReportState::default(),
            Self::DualShock4 => PadReportState::with_touchpad_height(dualshock4::TOUCHPAD_HEIGHT),
        }
    }

    pub fn input_report(self, state: &PadReportState, sequence: u8, elapsed: Duration) -> Vec<u8> {
        match self {
            Self::DualSense => state
                .input_report(sequence, dualsense::sensor_timestamp(elapsed))
                .to_vec(),
            Self::DualShock4 => {
                dualshock4::input_report(state, sequence, dualshock4::sensor_timestamp(elapsed))
                    .to_vec()
            }
        }
    }

    pub fn feature_report(self, id: u8, mac: [u8; 6]) -> Option<Vec<u8>> {
        match self {
            Self::DualSense => dualsense::feature_report(id, mac),
            Self::DualShock4 => dualshock4::feature_report(id, mac),
        }
    }

    pub fn parse_output(self, data: &[u8]) -> Option<OutputReport> {
        match self {
            Self::DualSense => dualsense::parse_output(data),
            Self::DualShock4 => dualshock4::parse_output(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_model_presents_its_own_usb_identity_and_descriptor() {
        assert_eq!(PadModel::DualSense.product(), 0x0ce6);
        assert_eq!(PadModel::DualShock4.product(), 0x09cc);
        assert_ne!(PadModel::DualSense.name(), PadModel::DualShock4.name());
        assert_ne!(
            PadModel::DualSense.descriptor(),
            PadModel::DualShock4.descriptor()
        );
    }

    #[test]
    fn input_reports_are_sixty_four_bytes_with_the_models_id() {
        let state = PadModel::DualSense.new_state();

        let dualsense = PadModel::DualSense.input_report(&state, 1, Duration::ZERO);
        let dualshock4 = PadModel::DualShock4.input_report(&state, 1, Duration::ZERO);

        assert_eq!(dualsense.len(), 64);
        assert_eq!(dualshock4.len(), 64);
        assert_eq!(dualsense[0], 0x01);
        assert_eq!(dualshock4[0], 0x01);
        assert_ne!(dualsense, dualshock4);
    }

    #[test]
    fn feature_requests_and_output_reports_follow_the_model() {
        let mac = [0; 6];

        assert!(PadModel::DualSense.feature_report(0x05, mac).is_some());
        assert!(PadModel::DualSense.feature_report(0x02, mac).is_none());
        assert!(PadModel::DualShock4.feature_report(0x02, mac).is_some());
        assert!(PadModel::DualShock4.feature_report(0x05, mac).is_none());

        let mut report = vec![0; 63];
        report[0] = 0x02;
        report[1] = 0x01;
        assert!(PadModel::DualSense.parse_output(&report).is_some());
        assert!(PadModel::DualShock4.parse_output(&report).is_none());
    }

    #[test]
    fn the_dualshock_four_pad_is_shorter_than_the_dualsense_one() {
        let mut dualsense = PadModel::DualSense.new_state();
        let mut dualshock4 = PadModel::DualShock4.new_state();
        let down = crate::GamepadEvent::TouchpadDown {
            finger: 0,
            x: 1.0,
            y: 1.0,
        };

        dualsense.apply(down).unwrap();
        dualshock4.apply(down).unwrap();

        assert_ne!(dualsense.contacts(), dualshock4.contacts());
    }
}
