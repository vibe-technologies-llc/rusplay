use std::{io, os::fd::OwnedFd, process, time::Instant};

use rustix::{
    fs::{Mode, OFlags, open},
    io::{Errno, read, write},
};

use crate::{
    GamepadEvent, InputError, PadFeedback,
    gamepad::{
        dualsense::{PadReportState, VENDOR_SONY},
        model::PadModel,
        pool::GamepadDevice,
    },
};

const DEVICE_PATH: &str = "/dev/uhid";
const EVENT_SIZE: usize = 4376;
const DATA_MAX: usize = 4096;
const BUS_USB: u16 = 3;
const OUTPUT_REPORT_TYPE: u8 = 1;
const REPLY_OK: u16 = 0;
const REPLY_IO_ERROR: u16 = 5;

const EVENT_DESTROY: u32 = 1;
const EVENT_START: u32 = 2;
const EVENT_STOP: u32 = 3;
const EVENT_OPEN: u32 = 4;
const EVENT_CLOSE: u32 = 5;
const EVENT_OUTPUT: u32 = 6;
const EVENT_GET_REPORT: u32 = 9;
const EVENT_GET_REPORT_REPLY: u32 = 10;
const EVENT_CREATE2: u32 = 11;
const EVENT_INPUT2: u32 = 12;
const EVENT_SET_REPORT: u32 = 13;
const EVENT_SET_REPORT_REPLY: u32 = 14;

const CREATE2_NAME: usize = 4;
const CREATE2_NAME_LEN: usize = 128;
const CREATE2_PHYS: usize = 132;
const CREATE2_PHYS_LEN: usize = 64;
const CREATE2_UNIQ: usize = 196;
const CREATE2_UNIQ_LEN: usize = 64;
const CREATE2_RD_SIZE: usize = 260;
const CREATE2_BUS: usize = 262;
const CREATE2_VENDOR: usize = 264;
const CREATE2_PRODUCT: usize = 268;
const CREATE2_VERSION: usize = 272;
const CREATE2_COUNTRY: usize = 276;
const CREATE2_RD_DATA: usize = 280;

const OUTPUT_DATA: usize = 4;
const OUTPUT_SIZE: usize = OUTPUT_DATA + DATA_MAX;
const OUTPUT_TYPE: usize = OUTPUT_SIZE + 2;
const REPORT_ID: usize = 4;
const REPORT_NUMBER: usize = 8;
const REPORT_TYPE: usize = 9;
const REPLY_ERROR: usize = 8;
const REPLY_SIZE: usize = 10;
const REPLY_DATA: usize = 12;
const INPUT2_SIZE: usize = 4;
const INPUT2_DATA: usize = 6;

pub(super) struct Identity<'a> {
    pub name: &'a str,
    pub phys: &'a str,
    pub uniq: &'a str,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u32,
    pub descriptor: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum KernelEvent {
    Start,
    Stop,
    Open,
    Close,
    Output {
        report_type: u8,
        data: Vec<u8>,
    },
    GetReport {
        id: u32,
        number: u8,
        report_type: u8,
    },
    SetReport {
        id: u32,
        number: u8,
        report_type: u8,
    },
    Other(u32),
}

fn blank(kind: u32) -> Vec<u8> {
    let mut event = vec![0; EVENT_SIZE];
    event[..4].copy_from_slice(&kind.to_ne_bytes());

    event
}

fn put(event: &mut [u8], offset: usize, bytes: &[u8], limit: usize) {
    let length = bytes.len().min(limit);
    event[offset..offset + length].copy_from_slice(&bytes[..length]);
}

fn text_limit(limit: usize) -> usize {
    limit - 1
}

pub(super) fn create2(identity: &Identity<'_>) -> Vec<u8> {
    let mut event = blank(EVENT_CREATE2);
    let descriptor = &identity.descriptor[..identity.descriptor.len().min(DATA_MAX)];
    let size = u16::try_from(descriptor.len()).unwrap_or(u16::MAX);

    put(
        &mut event,
        CREATE2_NAME,
        identity.name.as_bytes(),
        text_limit(CREATE2_NAME_LEN),
    );
    put(
        &mut event,
        CREATE2_PHYS,
        identity.phys.as_bytes(),
        text_limit(CREATE2_PHYS_LEN),
    );
    put(
        &mut event,
        CREATE2_UNIQ,
        identity.uniq.as_bytes(),
        text_limit(CREATE2_UNIQ_LEN),
    );
    put(&mut event, CREATE2_RD_SIZE, &size.to_ne_bytes(), 2);
    put(&mut event, CREATE2_BUS, &identity.bus.to_ne_bytes(), 2);
    put(
        &mut event,
        CREATE2_VENDOR,
        &u32::from(identity.vendor).to_ne_bytes(),
        4,
    );
    put(
        &mut event,
        CREATE2_PRODUCT,
        &u32::from(identity.product).to_ne_bytes(),
        4,
    );
    put(
        &mut event,
        CREATE2_VERSION,
        &identity.version.to_ne_bytes(),
        4,
    );
    put(&mut event, CREATE2_COUNTRY, &0u32.to_ne_bytes(), 4);
    put(&mut event, CREATE2_RD_DATA, descriptor, DATA_MAX);

    event
}

pub(super) fn input2(report: &[u8]) -> Vec<u8> {
    let mut event = blank(EVENT_INPUT2);
    let report = &report[..report.len().min(DATA_MAX)];
    let size = u16::try_from(report.len()).unwrap_or(u16::MAX);

    put(&mut event, INPUT2_SIZE, &size.to_ne_bytes(), 2);
    put(&mut event, INPUT2_DATA, report, DATA_MAX);

    event
}

pub(super) fn get_report_reply(id: u32, report: Option<&[u8]>) -> Vec<u8> {
    let mut event = blank(EVENT_GET_REPORT_REPLY);
    put(&mut event, REPORT_ID, &id.to_ne_bytes(), 4);

    match report {
        Some(report) => {
            let report = &report[..report.len().min(DATA_MAX)];
            let size = u16::try_from(report.len()).unwrap_or(u16::MAX);

            put(&mut event, REPLY_ERROR, &REPLY_OK.to_ne_bytes(), 2);
            put(&mut event, REPLY_SIZE, &size.to_ne_bytes(), 2);
            put(&mut event, REPLY_DATA, report, DATA_MAX);
        }
        None => put(&mut event, REPLY_ERROR, &REPLY_IO_ERROR.to_ne_bytes(), 2),
    }

    event
}

pub(super) fn set_report_reply(id: u32) -> Vec<u8> {
    let mut event = blank(EVENT_SET_REPORT_REPLY);
    put(&mut event, REPORT_ID, &id.to_ne_bytes(), 4);
    put(&mut event, REPLY_ERROR, &REPLY_OK.to_ne_bytes(), 2);

    event
}

fn destroy() -> Vec<u8> {
    blank(EVENT_DESTROY)
}

fn field<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], usize> {
    bytes
        .get(offset..offset + N)
        .and_then(|slice| slice.try_into().ok())
        .ok_or(bytes.len())
}

pub(super) fn decode(bytes: &[u8]) -> Result<KernelEvent, usize> {
    let kind = u32::from_ne_bytes(field(bytes, 0)?);

    Ok(match kind {
        EVENT_START => KernelEvent::Start,
        EVENT_STOP => KernelEvent::Stop,
        EVENT_OPEN => KernelEvent::Open,
        EVENT_CLOSE => KernelEvent::Close,
        EVENT_OUTPUT => {
            let size = usize::from(u16::from_ne_bytes(field(bytes, OUTPUT_SIZE)?)).min(DATA_MAX);
            let report_type = field::<1>(bytes, OUTPUT_TYPE)?[0];
            let data = bytes[OUTPUT_DATA..OUTPUT_DATA + size].to_vec();

            KernelEvent::Output { report_type, data }
        }
        EVENT_GET_REPORT => KernelEvent::GetReport {
            id: u32::from_ne_bytes(field(bytes, REPORT_ID)?),
            number: field::<1>(bytes, REPORT_NUMBER)?[0],
            report_type: field::<1>(bytes, REPORT_TYPE)?[0],
        },
        EVENT_SET_REPORT => KernelEvent::SetReport {
            id: u32::from_ne_bytes(field(bytes, REPORT_ID)?),
            number: field::<1>(bytes, REPORT_NUMBER)?[0],
            report_type: field::<1>(bytes, REPORT_TYPE)?[0],
        },
        other => KernelEvent::Other(other),
    })
}

pub(super) struct UhidDevice {
    slot: u8,
    fd: OwnedFd,
    buffer: Vec<u8>,
}

impl UhidDevice {
    pub fn open(slot: u8) -> Result<Self, InputError> {
        let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let fd = open(DEVICE_PATH, flags, Mode::empty()).map_err(|errno| {
            let source = io::Error::from(errno);

            if source.kind() == io::ErrorKind::PermissionDenied {
                InputError::UhidPermissionDenied { slot, source }
            } else {
                InputError::UhidOpen { slot, source }
            }
        })?;

        Ok(Self::from_fd(slot, fd))
    }

    pub fn from_fd(slot: u8, fd: OwnedFd) -> Self {
        Self {
            slot,
            fd,
            buffer: vec![0; EVENT_SIZE],
        }
    }

    pub fn send(&self, event: &[u8]) -> Result<(), InputError> {
        let fail = |source| InputError::UhidWrite {
            slot: self.slot,
            source,
        };

        let written = write(&self.fd, event)
            .map_err(io::Error::from)
            .map_err(fail)?;

        if written == event.len() {
            Ok(())
        } else {
            Err(fail(io::ErrorKind::WriteZero.into()))
        }
    }

    pub fn service(
        &mut self,
        model: PadModel,
        mac: [u8; 6],
    ) -> Result<Vec<KernelEvent>, InputError> {
        let mut events = Vec::new();

        loop {
            let length = match read(&self.fd, &mut self.buffer) {
                Ok(0) | Err(Errno::AGAIN) => return Ok(events),
                Ok(length) => length,
                Err(Errno::INTR) => continue,
                Err(errno) => {
                    return Err(InputError::UhidRead {
                        slot: self.slot,
                        source: errno.into(),
                    });
                }
            };

            let event =
                decode(&self.buffer[..length]).map_err(|len| InputError::UhidEventTruncated {
                    slot: self.slot,
                    len,
                })?;
            self.answer(&event, model, mac)?;
            events.push(event);
        }
    }

    fn answer(&self, event: &KernelEvent, model: PadModel, mac: [u8; 6]) -> Result<(), InputError> {
        match event {
            KernelEvent::GetReport { id, number, .. } => {
                let report = model.feature_report(*number, mac);

                self.send(&get_report_reply(*id, report.as_deref()))
            }
            KernelEvent::SetReport { id, .. } => self.send(&set_report_reply(*id)),
            _ => Ok(()),
        }
    }
}

impl Drop for UhidDevice {
    fn drop(&mut self) {
        self.send(&destroy()).ok();
    }
}

fn mac_for(slot: u8) -> [u8; 6] {
    let [.., high, low] = process::id().to_be_bytes();

    [0x02, b'r', b'p', high, low, slot]
}

pub struct UhidGamepad {
    slot: u8,
    model: PadModel,
    device: UhidDevice,
    mac: [u8; 6],
    state: PadReportState,
    sequence: u8,
    epoch: Instant,
}

impl UhidGamepad {
    pub fn dualsense(slot: u8) -> Result<Self, InputError> {
        Self::create(slot, PadModel::DualSense)
    }

    pub fn dualshock4(slot: u8) -> Result<Self, InputError> {
        Self::create(slot, PadModel::DualShock4)
    }

    fn create(slot: u8, model: PadModel) -> Result<Self, InputError> {
        let device = UhidDevice::open(slot)?;

        Self::with_device(slot, model, device)
    }

    fn with_device(slot: u8, model: PadModel, device: UhidDevice) -> Result<Self, InputError> {
        let mac = mac_for(slot);
        let uniq = mac.map(|byte| format!("{byte:02x}")).join(":");
        let phys = format!("rusplay/uhid{slot}");

        device.send(&create2(&Identity {
            name: model.name(),
            phys: &phys,
            uniq: &uniq,
            bus: BUS_USB,
            vendor: VENDOR_SONY,
            product: model.product(),
            version: model.version(),
            descriptor: model.descriptor(),
        }))?;

        Ok(Self {
            slot,
            model,
            device,
            mac,
            state: model.new_state(),
            sequence: 0,
            epoch: Instant::now(),
        })
    }

    fn send_report(&mut self) -> Result<(), InputError> {
        let report = self
            .model
            .input_report(&self.state, self.sequence, self.epoch.elapsed());
        self.sequence = self.sequence.wrapping_add(1);

        self.device.send(&input2(&report))
    }

    fn service(&mut self) -> Result<Vec<KernelEvent>, InputError> {
        self.device.service(self.model, self.mac)
    }
}

impl GamepadDevice for UhidGamepad {
    fn handle(&mut self, event: GamepadEvent) -> Result<(), InputError> {
        if self.state.apply(event)? {
            self.send_report()
        } else {
            Ok(())
        }
    }

    fn poll_feedback(&mut self, out: &mut Vec<PadFeedback>) -> Result<(), InputError> {
        for event in self.service()? {
            if let KernelEvent::Output { report_type, data } = event
                && report_type == OUTPUT_REPORT_TYPE
                && let Some(report) = self.model.parse_output(&data)
            {
                report.feedback(self.slot, out);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::net::UnixDatagram,
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        GamepadAxis, GamepadButton, RumbleCommand,
        gamepad::{
            dualsense::{
                CALIBRATION_REPORT_ID, FIRMWARE_REPORT_ID, INPUT_REPORT_ID, INPUT_REPORT_LEN,
                OUTPUT_REPORT_ID, OUTPUT_REPORT_LEN, PAIRING_REPORT_ID,
            },
            dualshock4,
        },
        hardware,
    };

    fn pair() -> (UhidDevice, UnixDatagram) {
        let (ours, kernel) = UnixDatagram::pair().unwrap();
        ours.set_nonblocking(true).unwrap();

        (UhidDevice::from_fd(3, ours.into()), kernel)
    }

    fn kernel_event(kind: u32) -> Vec<u8> {
        blank(kind)
    }

    fn get_report(id: u32, number: u8) -> Vec<u8> {
        let mut event = kernel_event(EVENT_GET_REPORT);
        event[REPORT_ID..REPORT_ID + 4].copy_from_slice(&id.to_ne_bytes());
        event[REPORT_NUMBER] = number;
        event[REPORT_TYPE] = 0;

        event
    }

    fn output_event(report_type: u8, data: &[u8]) -> Vec<u8> {
        let mut event = kernel_event(EVENT_OUTPUT);
        event[OUTPUT_DATA..OUTPUT_DATA + data.len()].copy_from_slice(data);
        event[OUTPUT_SIZE..OUTPUT_SIZE + 2]
            .copy_from_slice(&u16::try_from(data.len()).unwrap().to_ne_bytes());
        event[OUTPUT_TYPE] = report_type;

        event
    }

    fn receive(kernel: &UnixDatagram) -> Vec<u8> {
        let mut buffer = vec![0; EVENT_SIZE];
        let length = kernel.recv(&mut buffer).unwrap();
        buffer.truncate(length);

        buffer
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_ne_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn create2_places_every_field_at_the_kernel_offset() {
        let event = create2(&Identity {
            name: "pad",
            phys: "phys",
            uniq: "uniq",
            bus: 3,
            vendor: 0x054c,
            product: 0x0ce6,
            version: 0x0100,
            descriptor: &[1, 2, 3],
        });

        assert_eq!(event.len(), 4376);
        assert_eq!(u32_at(&event, 0), 11);
        assert_eq!(&event[4..8], b"pad\0");
        assert_eq!(&event[132..137], b"phys\0");
        assert_eq!(&event[196..201], b"uniq\0");
        assert_eq!(u16_at(&event, 260), 3);
        assert_eq!(u16_at(&event, 262), 3);
        assert_eq!(u32_at(&event, 264), 0x054c);
        assert_eq!(u32_at(&event, 268), 0x0ce6);
        assert_eq!(u32_at(&event, 272), 0x0100);
        assert_eq!(u32_at(&event, 276), 0);
        assert_eq!(&event[280..284], &[1, 2, 3, 0]);
    }

    #[test]
    fn create2_leaves_room_for_the_terminating_zero_of_long_names() {
        let long = "x".repeat(300);

        let event = create2(&Identity {
            name: &long,
            phys: &long,
            uniq: &long,
            bus: 3,
            vendor: 1,
            product: 2,
            version: 3,
            descriptor: &[],
        });

        assert_eq!(event[4 + 126], b'x');
        assert_eq!(event[4 + 127], 0);
        assert_eq!(event[132 + 62], b'x');
        assert_eq!(event[132 + 63], 0);
        assert_eq!(event[196 + 62], b'x');
        assert_eq!(event[196 + 63], 0);
    }

    #[test]
    fn every_models_descriptor_fits_the_create2_buffer() {
        for model in [PadModel::DualSense, PadModel::DualShock4] {
            let descriptor = model.descriptor();

            let event = create2(&Identity {
                name: model.name(),
                phys: "",
                uniq: "",
                bus: BUS_USB,
                vendor: VENDOR_SONY,
                product: model.product(),
                version: model.version(),
                descriptor,
            });

            assert_eq!(usize::from(u16_at(&event, 260)), descriptor.len());
            assert_eq!(&event[280..280 + descriptor.len()], descriptor);
        }
    }

    #[test]
    fn input2_carries_its_size_then_the_report() {
        let event = input2(&[1, 9, 8, 7]);

        assert_eq!(event.len(), 4376);
        assert_eq!(u32_at(&event, 0), 12);
        assert_eq!(u16_at(&event, 4), 4);
        assert_eq!(&event[6..10], &[1, 9, 8, 7]);
    }

    #[test]
    fn replies_name_the_request_and_carry_the_report_or_an_error() {
        let ok = get_report_reply(77, Some(&[5, 6]));
        let failed = get_report_reply(78, None);
        let set = set_report_reply(79);

        assert_eq!(u32_at(&ok, 0), 10);
        assert_eq!(u32_at(&ok, 4), 77);
        assert_eq!(u16_at(&ok, 8), 0);
        assert_eq!(u16_at(&ok, 10), 2);
        assert_eq!(&ok[12..14], &[5, 6]);
        assert_eq!(u32_at(&failed, 4), 78);
        assert_eq!(u16_at(&failed, 8), 5);
        assert_eq!(u16_at(&failed, 10), 0);
        assert_eq!(u32_at(&set, 0), 14);
        assert_eq!(u32_at(&set, 4), 79);
        assert_eq!(u16_at(&set, 8), 0);
    }

    #[test]
    fn kernel_events_decode_and_truncated_ones_are_refused() {
        let output = output_event(1, &[2, 0, 0]);

        assert_eq!(
            decode(&output).unwrap(),
            KernelEvent::Output {
                report_type: 1,
                data: vec![2, 0, 0]
            }
        );
        assert_eq!(
            decode(&get_report(9, 0x20)).unwrap(),
            KernelEvent::GetReport {
                id: 9,
                number: 0x20,
                report_type: 0
            }
        );
        assert_eq!(decode(&kernel_event(2)).unwrap(), KernelEvent::Start);
        assert_eq!(decode(&kernel_event(4)).unwrap(), KernelEvent::Open);
        assert_eq!(decode(&kernel_event(40)).unwrap(), KernelEvent::Other(40));
        assert_eq!(decode(&[1, 2]), Err(2));
        assert_eq!(decode(&output[..100]), Err(100));
        assert_eq!(decode(&get_report(1, 5)[..9]), Err(9));
    }

    #[test]
    fn an_output_size_beyond_the_buffer_is_clamped() {
        let mut event = kernel_event(EVENT_OUTPUT);
        event[OUTPUT_SIZE..OUTPUT_SIZE + 2].copy_from_slice(&u16::MAX.to_ne_bytes());

        let KernelEvent::Output { data, .. } = decode(&event).unwrap() else {
            panic!("not an output event");
        };

        assert_eq!(data.len(), DATA_MAX);
    }

    #[test]
    fn feature_requests_are_answered_during_service() {
        let (mut device, kernel) = pair();
        let mac = [2, 3, 4, 5, 6, 7];

        kernel.send(&get_report(11, PAIRING_REPORT_ID)).unwrap();
        kernel.send(&get_report(12, CALIBRATION_REPORT_ID)).unwrap();
        kernel.send(&get_report(13, FIRMWARE_REPORT_ID)).unwrap();
        kernel.send(&get_report(14, 0x81)).unwrap();
        let events = device.service(PadModel::DualSense, mac).unwrap();

        let pairing = receive(&kernel);
        let calibration = receive(&kernel);
        let firmware = receive(&kernel);
        let unknown = receive(&kernel);
        assert_eq!(events.len(), 4);
        assert_eq!(u32_at(&pairing, 4), 11);
        assert_eq!(u16_at(&pairing, 10), 20);
        assert_eq!(&pairing[12..19], &[9, 2, 3, 4, 5, 6, 7]);
        assert_eq!(u16_at(&calibration, 10), 41);
        assert_eq!(calibration[12], CALIBRATION_REPORT_ID);
        assert_eq!(u16_at(&firmware, 10), 64);
        assert_eq!(firmware[12], FIRMWARE_REPORT_ID);
        assert_eq!(u16_at(&unknown, 8), 5);
    }

    #[test]
    fn set_report_requests_are_acknowledged() {
        let (mut device, kernel) = pair();
        let mut request = kernel_event(EVENT_SET_REPORT);
        request[REPORT_ID..REPORT_ID + 4].copy_from_slice(&21u32.to_ne_bytes());

        kernel.send(&request).unwrap();
        device.service(PadModel::DualSense, [0; 6]).unwrap();

        let reply = receive(&kernel);
        assert_eq!(u32_at(&reply, 0), 14);
        assert_eq!(u32_at(&reply, 4), 21);
    }

    #[test]
    fn service_with_nothing_pending_returns_no_events() {
        let (mut device, _kernel) = pair();

        let events = device.service(PadModel::DualSense, [0; 6]).unwrap();

        assert_eq!(events.len(), 0);
    }

    #[test]
    fn a_short_event_from_the_kernel_is_an_error() {
        let (mut device, kernel) = pair();

        kernel.send(&[1, 2]).unwrap();
        let result = device.service(PadModel::DualSense, [0; 6]);

        assert!(matches!(
            result,
            Err(InputError::UhidEventTruncated { slot: 3, len: 2 })
        ));
    }

    #[test]
    fn dropping_the_device_destroys_it() {
        let (device, kernel) = pair();

        drop(device);

        assert_eq!(u32_at(&receive(&kernel), 0), 1);
    }

    fn gamepad_of(model: PadModel) -> (UhidGamepad, UnixDatagram) {
        let (device, kernel) = pair();
        let gamepad = UhidGamepad::with_device(3, model, device).unwrap();

        (gamepad, kernel)
    }

    fn gamepad() -> (UhidGamepad, UnixDatagram) {
        gamepad_of(PadModel::DualSense)
    }

    #[test]
    fn creating_a_gamepad_announces_a_usb_dualsense() {
        let (_gamepad, kernel) = gamepad();

        let event = receive(&kernel);

        assert_eq!(u32_at(&event, 0), 11);
        assert!(event[4..].starts_with(PadModel::DualSense.name().as_bytes()));
        assert_eq!(u16_at(&event, 262), 3);
        assert_eq!(u32_at(&event, 264), 0x054c);
        assert_eq!(u32_at(&event, 268), 0x0ce6);
        assert!(event[196..].starts_with(b"02:72:70:"));
        assert!(event[196..].windows(3).any(|window| window == b":03"));
    }

    #[test]
    fn creating_a_dualshock_four_announces_a_usb_dualshock_four() {
        let (_gamepad, kernel) = gamepad_of(PadModel::DualShock4);

        let event = receive(&kernel);

        assert_eq!(u32_at(&event, 0), 11);
        assert!(event[4..].starts_with(PadModel::DualShock4.name().as_bytes()));
        assert_eq!(u16_at(&event, 262), 3);
        assert_eq!(u32_at(&event, 264), 0x054c);
        assert_eq!(u32_at(&event, 268), 0x09cc);
        assert_eq!(
            usize::from(u16_at(&event, 260)),
            dualshock4::REPORT_DESCRIPTOR.len()
        );
    }

    #[test]
    fn a_dualshock_four_event_sends_its_own_input_report() {
        let (mut gamepad, kernel) = gamepad_of(PadModel::DualShock4);
        receive(&kernel);

        gamepad
            .handle(GamepadEvent::Axis {
                axis: GamepadAxis::LEFT_TRIGGER,
                value: 1.0,
            })
            .unwrap();

        let event = receive(&kernel);
        assert_eq!(u32_at(&event, 0), 12);
        assert_eq!(usize::from(u16_at(&event, 4)), dualshock4::INPUT_REPORT_LEN);
        assert_eq!(event[6], dualshock4::INPUT_REPORT_ID);
        assert_eq!(event[6 + 8], 255);
        assert_eq!(event[6 + 6] & 0x04, 0x04);
    }

    #[test]
    fn a_dualshock_four_answers_its_three_feature_reports_and_nothing_else() {
        let (mut gamepad, kernel) = gamepad_of(PadModel::DualShock4);
        receive(&kernel);

        for (id, number) in [(1, 0x12), (2, 0x02), (3, 0xa3), (4, 0x05)] {
            kernel.send(&get_report(id, number)).unwrap();
        }
        gamepad.service().unwrap();

        let sizes: Vec<_> = (0..4)
            .map(|_| {
                let reply = receive(&kernel);
                (u16_at(&reply, 8), u16_at(&reply, 10))
            })
            .collect();
        assert_eq!(sizes, vec![(0, 16), (0, 37), (0, 49), (5, 0)]);
    }

    #[test]
    fn dualshock_four_output_reports_become_feedback_for_the_slot() {
        let (mut gamepad, kernel) = gamepad_of(PadModel::DualShock4);
        let mut report = vec![0; dualshock4::OUTPUT_REPORT_LEN];
        report[0] = dualshock4::OUTPUT_REPORT_ID;
        report[1] = 0x03;
        report[4] = 0x80;
        report[5] = 0xff;
        report[6..9].copy_from_slice(&[9, 8, 7]);
        let mut out = Vec::new();

        kernel.send(&output_event(1, &report)).unwrap();
        kernel.send(&output_event(1, &report[..1])).unwrap();
        gamepad.poll_feedback(&mut out).unwrap();

        assert_eq!(
            out,
            vec![
                PadFeedback::Rumble(RumbleCommand {
                    slot: 3,
                    strong: 0xffff,
                    weak: 0x8080,
                    delay_ms: 0,
                    duration_ms: 0
                }),
                PadFeedback::Lightbar {
                    slot: 3,
                    red: 9,
                    green: 8,
                    blue: 7
                },
            ]
        );
    }

    #[test]
    fn a_pad_event_that_changes_the_report_sends_one_input_report() {
        let (mut gamepad, kernel) = gamepad();
        receive(&kernel);

        gamepad
            .handle(GamepadEvent::Button {
                button: GamepadButton::SOUTH,
                pressed: true,
            })
            .unwrap();
        gamepad
            .handle(GamepadEvent::Button {
                button: GamepadButton::SOUTH,
                pressed: true,
            })
            .unwrap();
        gamepad.handle(GamepadEvent::Connected).unwrap();

        let event = receive(&kernel);
        assert_eq!(u32_at(&event, 0), 12);
        assert_eq!(usize::from(u16_at(&event, 4)), INPUT_REPORT_LEN);
        assert_eq!(event[6], INPUT_REPORT_ID);
        assert_eq!(event[6 + 8] & 0xf0, 0x20);
        kernel.set_nonblocking(true).unwrap();
        assert!(kernel.recv(&mut [0; 16]).is_err());
    }

    #[test]
    fn output_reports_become_feedback_for_the_slot() {
        let (mut gamepad, kernel) = gamepad();
        let mut report = vec![0; OUTPUT_REPORT_LEN];
        report[0] = OUTPUT_REPORT_ID;
        report[1] = 0x03;
        report[3] = 0x80;
        report[4] = 0xff;
        let mut lightbar_only = vec![0; OUTPUT_REPORT_LEN];
        lightbar_only[0] = OUTPUT_REPORT_ID;
        lightbar_only[2] = 0x04;
        lightbar_only[45..48].copy_from_slice(&[0, 0, 128]);
        let mut out = Vec::new();

        kernel.send(&output_event(1, &report)).unwrap();
        kernel.send(&output_event(1, &lightbar_only)).unwrap();
        kernel.send(&output_event(0, &report)).unwrap();
        gamepad.poll_feedback(&mut out).unwrap();

        assert_eq!(
            out,
            vec![
                PadFeedback::Rumble(RumbleCommand {
                    slot: 3,
                    strong: 0xffff,
                    weak: 0x8080,
                    delay_ms: 0,
                    duration_ms: 0
                }),
                PadFeedback::Lightbar {
                    slot: 3,
                    red: 0,
                    green: 0,
                    blue: 128
                },
            ]
        );
    }

    #[test]
    fn zero_motors_make_a_stop_command() {
        let (mut gamepad, kernel) = gamepad();
        let mut report = vec![0; OUTPUT_REPORT_LEN];
        report[0] = OUTPUT_REPORT_ID;
        report[1] = 0x01;
        let mut out = Vec::new();

        kernel.send(&output_event(1, &report)).unwrap();
        gamepad.poll_feedback(&mut out).unwrap();

        assert_eq!(out, vec![PadFeedback::Rumble(RumbleCommand::stop(3))]);
    }

    #[test]
    fn the_mac_differs_per_slot_and_is_locally_administered() {
        assert_ne!(mac_for(0), mac_for(1));
        assert_eq!(mac_for(5)[0] & 0x03, 0x02);
        assert_eq!(mac_for(5)[5], 5);
    }

    fn probe_live_feature_requests(model: PadModel, expected: [u8; 3]) {
        let device = match UhidDevice::open(0) {
            Ok(device) => device,
            Err(error) => {
                hardware::missing(error);
                return;
            }
        };
        if !hardware::has_module("hid_playstation") {
            return;
        }
        let mut pad = UhidGamepad::with_device(0, model, device).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut requested = Vec::new();

        while Instant::now() < deadline && requested.len() < 3 {
            for event in pad.service().unwrap() {
                if let KernelEvent::GetReport { number, .. } = event {
                    requested.push(number);
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        requested.sort_unstable();

        assert_eq!(requested, expected.to_vec());
    }

    #[test]
    fn live_kernel_driver_probes_the_three_feature_reports() {
        probe_live_feature_requests(
            PadModel::DualSense,
            [CALIBRATION_REPORT_ID, PAIRING_REPORT_ID, FIRMWARE_REPORT_ID],
        );
    }

    #[test]
    fn live_kernel_driver_probes_the_three_dualshock_four_feature_reports() {
        probe_live_feature_requests(
            PadModel::DualShock4,
            [
                dualshock4::CALIBRATION_REPORT_ID,
                dualshock4::PAIRING_REPORT_ID,
                dualshock4::FIRMWARE_REPORT_ID,
            ],
        );
    }
}
