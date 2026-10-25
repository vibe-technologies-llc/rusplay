use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputEvent as EvdevEvent, InputId,
    UinputAbsSetup, uinput::VirtualDevice,
};
use rusplay_config::InputConfig;

use crate::{
    InputError,
    gamepad::{
        pool::{GamepadDevice, GamepadFactory, GamepadPool},
        xbox360::{HAT_MAX, KEYS, Report, STICK_MAX, STICK_MIN, TRIGGER_MAX},
    },
};

const NAME: &str = "Microsoft X-Box 360 pad";
const VENDOR_MICROSOFT: u16 = 0x045e;
const PRODUCT_XBOX_360: u16 = 0x028e;
const VERSION_XBOX_360: u16 = 0x0110;
const STICK_FUZZ: i32 = 16;
const STICK_FLAT: i32 = 128;

pub struct UinputGamepad {
    slot: u8,
    device: VirtualDevice,
}

impl UinputGamepad {
    pub fn create(slot: u8) -> Result<Self, InputError> {
        let create = |source| InputError::UinputCreate { slot, source };

        let mut keys = AttributeSet::<evdev::KeyCode>::new();
        for key in KEYS {
            keys.insert(key);
        }

        let stick = AbsInfo::new(0, STICK_MIN, STICK_MAX, STICK_FUZZ, STICK_FLAT, 0);
        let trigger = AbsInfo::new(0, 0, TRIGGER_MAX, 0, 0, 0);
        let hat = AbsInfo::new(0, -HAT_MAX, HAT_MAX, 0, 0, 0);
        let axes = [
            (AbsoluteAxisCode::ABS_X, stick),
            (AbsoluteAxisCode::ABS_Y, stick),
            (AbsoluteAxisCode::ABS_Z, trigger),
            (AbsoluteAxisCode::ABS_RX, stick),
            (AbsoluteAxisCode::ABS_RY, stick),
            (AbsoluteAxisCode::ABS_RZ, trigger),
            (AbsoluteAxisCode::ABS_HAT0X, hat),
            (AbsoluteAxisCode::ABS_HAT0Y, hat),
        ];

        let mut builder = VirtualDevice::builder()
            .map_err(create)?
            .name(NAME)
            .input_id(InputId::new(
                BusType::BUS_USB,
                VENDOR_MICROSOFT,
                PRODUCT_XBOX_360,
                VERSION_XBOX_360,
            ))
            .with_keys(&keys)
            .map_err(create)?;
        for (code, info) in axes {
            builder = builder
                .with_absolute_axis(&UinputAbsSetup::new(code, info))
                .map_err(create)?;
        }

        Ok(Self {
            slot,
            device: builder.build().map_err(create)?,
        })
    }

    pub fn device_mut(&mut self) -> &mut VirtualDevice {
        &mut self.device
    }
}

impl GamepadDevice for UinputGamepad {
    fn emit(&mut self, report: Report) -> Result<(), InputError> {
        let event = match report {
            Report::Key { code, value } => EvdevEvent::new(EventType::KEY.0, code, value),
            Report::Abs { code, value } => EvdevEvent::new(EventType::ABSOLUTE.0, code, value),
        };

        self.device
            .emit(&[event])
            .map_err(|source| InputError::UinputEmit {
                slot: self.slot,
                source,
            })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct UinputFactory;

impl GamepadFactory for UinputFactory {
    type Device = UinputGamepad;

    fn create(&mut self, slot: u8) -> Result<UinputGamepad, InputError> {
        UinputGamepad::create(slot)
    }
}

pub type UinputGamepads = GamepadPool<UinputFactory>;

impl UinputGamepads {
    pub fn from_config(config: InputConfig) -> Self {
        Self::new(UinputFactory, config.max_gamepads)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        path::Path,
        thread,
        time::{Duration, Instant},
    };

    use evdev::{Device, KeyCode};

    use super::*;
    use crate::{GamepadAxis, GamepadButton, GamepadEvent, InputEvent, InputSink};

    fn open_when_permitted(node: &Path) -> Device {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match Device::open(node) {
                Ok(device) => return device,
                Err(error) if Instant::now() < deadline => {
                    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => panic!("opening {} failed: {error}", node.display()),
            }
        }
    }

    #[test]
    #[ignore = "needs write access to /dev/uinput"]
    fn live_device_advertises_the_xbox_360_layout() {
        let mut gamepad = UinputGamepad::create(0).unwrap();

        let node = gamepad
            .device_mut()
            .enumerate_dev_nodes_blocking()
            .unwrap()
            .filter_map(Result::ok)
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("event"))
            })
            .unwrap();
        let observed = open_when_permitted(&node);

        let id = observed.input_id();
        let keys = observed.supported_keys().unwrap();
        let axes = observed.supported_absolute_axes().unwrap();

        assert_eq!(observed.name(), Some(NAME));
        assert_eq!(id.vendor(), VENDOR_MICROSOFT);
        assert_eq!(id.product(), PRODUCT_XBOX_360);
        assert!(keys.contains(KeyCode::BTN_SOUTH) && keys.contains(KeyCode::BTN_THUMBR));
        assert!(
            axes.contains(AbsoluteAxisCode::ABS_HAT0Y) && axes.contains(AbsoluteAxisCode::ABS_RZ)
        );
    }

    #[test]
    #[ignore = "needs write access to /dev/uinput"]
    fn live_pool_creates_and_drives_virtual_pads() {
        let mut pads = UinputGamepads::from_config(InputConfig::default());

        pads.inject(InputEvent::Gamepad {
            slot: 1,
            event: GamepadEvent::Connected,
        })
        .unwrap();
        pads.inject(InputEvent::Gamepad {
            slot: 1,
            event: GamepadEvent::Button {
                button: GamepadButton::SOUTH,
                pressed: true,
            },
        })
        .unwrap();
        pads.inject(InputEvent::Gamepad {
            slot: 1,
            event: GamepadEvent::Axis {
                axis: GamepadAxis::LEFT_X,
                value: 0.5,
            },
        })
        .unwrap();
        pads.inject(InputEvent::Gamepad {
            slot: 1,
            event: GamepadEvent::Disconnected,
        })
        .unwrap();

        assert!(!pads.is_connected(1));
    }
}
