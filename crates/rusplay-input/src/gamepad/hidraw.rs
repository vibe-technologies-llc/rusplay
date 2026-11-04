use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::{
    TRIGGER_EFFECT_LEN,
    gamepad::dualsense::{PRODUCT_DUALSENSE, VENDOR_SONY},
};

const PRODUCT_DUALSENSE_EDGE: u16 = 0x0df2;
const BUS_USB: u16 = 0x0003;
const BUS_BLUETOOTH: u16 = 0x0005;

const HIDRAW_DIRECTORY: &str = "hidraw";
const HIDRAW_PREFIX: &str = "hidraw";

const USB_REPORT_ID: u8 = 0x02;
const USB_REPORT_LEN: usize = 63;
const USB_COMMON_OFFSET: usize = 1;

const BLUETOOTH_REPORT_ID: u8 = 0x31;
const BLUETOOTH_REPORT_LEN: usize = 78;
const BLUETOOTH_COMMON_OFFSET: usize = 3;
const BLUETOOTH_TAG: u8 = 0x10;
const BLUETOOTH_SEQUENCE_SHIFT: u8 = 4;
const BLUETOOTH_CRC_SEED: u8 = 0xa2;
const CRC_LEN: usize = 4;

const COMMON_VALID0: usize = 0;
const COMMON_RIGHT_TRIGGER: usize = 10;
const COMMON_LEFT_TRIGGER: usize = 21;
const VALID0_RIGHT_TRIGGER: u8 = 0x04;
const VALID0_LEFT_TRIGGER: u8 = 0x08;

const CRC32_POLYNOMIAL: u32 = 0xedb8_8320;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HidBus {
    Usb,
    Bluetooth,
}

pub(super) fn dualsense_bus(hid_device_name: &str) -> Option<HidBus> {
    let (ids, _instance) = hid_device_name.split_once('.')?;
    let mut parts = ids
        .split(':')
        .map(|part| u16::from_str_radix(part, 16).ok());
    let (bus, vendor, product) = (parts.next()??, parts.next()??, parts.next()??);

    if parts.next().is_some()
        || vendor != VENDOR_SONY
        || ![PRODUCT_DUALSENSE, PRODUCT_DUALSENSE_EDGE].contains(&product)
    {
        return None;
    }

    match bus {
        BUS_USB => Some(HidBus::Usb),
        BUS_BLUETOOTH => Some(HidBus::Bluetooth),
        _ => None,
    }
}

pub(super) fn hid_device_bus(hid_directory: &Path) -> Option<HidBus> {
    dualsense_bus(hid_directory.file_name().and_then(OsStr::to_str)?)
}

pub(super) fn hidraw_node(hid_directory: &Path, dev_root: &Path) -> Option<PathBuf> {
    fs::read_dir(hid_directory.join(HIDRAW_DIRECTORY))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with(HIDRAW_PREFIX))
        .min()
        .map(|name| dev_root.join(name))
}

pub(super) fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = u32::MAX;

    for byte in parts.iter().flat_map(|part| part.iter()) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let carry = crc & 1;
            crc >>= 1;
            if carry != 0 {
                crc ^= CRC32_POLYNOMIAL;
            }
        }
    }

    !crc
}

pub(super) fn trigger_report(
    bus: HidBus,
    right: bool,
    effect: [u8; TRIGGER_EFFECT_LEN],
    sequence: u8,
) -> Vec<u8> {
    let (flag, offset) = if right {
        (VALID0_RIGHT_TRIGGER, COMMON_RIGHT_TRIGGER)
    } else {
        (VALID0_LEFT_TRIGGER, COMMON_LEFT_TRIGGER)
    };
    let (length, common) = match bus {
        HidBus::Usb => (USB_REPORT_LEN, USB_COMMON_OFFSET),
        HidBus::Bluetooth => (BLUETOOTH_REPORT_LEN, BLUETOOTH_COMMON_OFFSET),
    };

    let mut report = vec![0; length];
    report[common + COMMON_VALID0] = flag;
    report[common + offset..common + offset + TRIGGER_EFFECT_LEN].copy_from_slice(&effect);

    match bus {
        HidBus::Usb => report[0] = USB_REPORT_ID,
        HidBus::Bluetooth => {
            report[0] = BLUETOOTH_REPORT_ID;
            report[1] = (sequence & 0x0f) << BLUETOOTH_SEQUENCE_SHIFT;
            report[2] = BLUETOOTH_TAG;

            let body = length - CRC_LEN;
            let crc = crc32(&[&[BLUETOOTH_CRC_SEED], &report[..body]]);
            report[body..].copy_from_slice(&crc.to_le_bytes());
        }
    }

    report
}

pub(super) fn send(node: &Path, report: &[u8]) -> io::Result<()> {
    OpenOptions::new().write(true).open(node)?.write_all(report)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    const EFFECT: [u8; TRIGGER_EFFECT_LEN] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(&[b"123456789"]), 0xcbf4_3926);
        assert_eq!(crc32(&[b"12345", b"6789"]), 0xcbf4_3926);
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn usb_reports_hold_one_side_and_its_flag() {
        let right = trigger_report(HidBus::Usb, true, EFFECT, 5);
        let left = trigger_report(HidBus::Usb, false, EFFECT, 5);

        assert_eq!(right.len(), 63);
        assert_eq!(right[0], 0x02);
        assert_eq!(right[1], 0x04);
        assert_eq!(&right[11..22], &EFFECT);
        assert!(right[22..].iter().all(|byte| *byte == 0));
        assert_eq!(left.len(), 63);
        assert_eq!(left[1], 0x08);
        assert_eq!(&left[22..33], &EFFECT);
        assert!(left[11..22].iter().all(|byte| *byte == 0));
        assert_eq!(left[2], 0);
        assert_eq!(left[3], 0);
        assert_eq!(left[4], 0);
    }

    #[test]
    fn bluetooth_reports_carry_a_sequence_tag_and_a_seeded_crc() {
        let report = trigger_report(HidBus::Bluetooth, true, EFFECT, 0x13);

        assert_eq!(report.len(), 78);
        assert_eq!(report[0], 0x31);
        assert_eq!(report[1], 0x30);
        assert_eq!(report[2], 0x10);
        assert_eq!(report[3], 0x04);
        assert_eq!(&report[13..24], &EFFECT);
        assert!(report[24..74].iter().all(|byte| *byte == 0));

        let expected = crc32(&[&[0xa2], &report[..74]]);
        assert_eq!(&report[74..], &expected.to_le_bytes());
        assert_ne!(expected, crc32(&[&report[..74]]));
    }

    #[test]
    fn bluetooth_crc_changes_with_the_payload_and_the_sequence() {
        let one = trigger_report(HidBus::Bluetooth, true, EFFECT, 1);
        let two = trigger_report(HidBus::Bluetooth, true, EFFECT, 2);
        let left = trigger_report(HidBus::Bluetooth, false, EFFECT, 1);

        assert_ne!(one[74..], two[74..]);
        assert_ne!(one[74..], left[74..]);
        assert_eq!(left[3], 0x08);
        assert_eq!(&left[24..35], &EFFECT);
    }

    #[test]
    fn only_dualsense_hid_devices_are_recognised_with_their_bus() {
        assert_eq!(dualsense_bus("0003:054C:0CE6.0005"), Some(HidBus::Usb));
        assert_eq!(
            dualsense_bus("0005:054C:0CE6.000A"),
            Some(HidBus::Bluetooth)
        );
        assert_eq!(dualsense_bus("0003:054C:0DF2.0001"), Some(HidBus::Usb));
        assert_eq!(dualsense_bus("0003:054C:09CC.0001"), None);
        assert_eq!(dualsense_bus("0003:045E:028E.0001"), None);
        assert_eq!(dualsense_bus("0018:054C:0CE6.0001"), None);
        assert_eq!(dualsense_bus("0003:054C:0CE6:0001.0001"), None);
        assert_eq!(dualsense_bus("0003:054C.0001"), None);
        assert_eq!(dualsense_bus("input3"), None);
        assert_eq!(dualsense_bus(""), None);
    }

    #[test]
    fn the_hidraw_node_comes_from_the_hid_device_directory() {
        let directory = tempfile::tempdir().unwrap();
        let hid = directory.path().join("0003:054C:0CE6.0005");
        fs::create_dir_all(hid.join("hidraw/hidraw7")).unwrap();
        fs::create_dir_all(hid.join("hidraw/other")).unwrap();
        let dev = directory.path().join("dev");

        let found = hidraw_node(&hid, &dev);
        let missing = hidraw_node(&directory.path().join("0003:054C:0CE6.0006"), &dev);

        assert_eq!(found, Some(dev.join("hidraw7")));
        assert_eq!(missing, None);
    }

    #[test]
    fn a_report_is_written_whole_to_the_node() {
        let directory = tempfile::tempdir().unwrap();
        let node = directory.path().join("hidraw3");
        fs::write(&node, []).unwrap();
        let report = trigger_report(HidBus::Usb, false, EFFECT, 0);

        send(&node, &report).unwrap();

        assert_eq!(fs::read(&node).unwrap(), report);
    }

    #[test]
    fn a_node_that_cannot_be_opened_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("hidraw3");
        symlink(directory.path().join("nowhere"), &link).unwrap();

        let result = send(&link, &[0x02]);

        assert!(result.is_err());
    }
}
