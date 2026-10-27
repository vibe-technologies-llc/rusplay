use crate::{GamepadAxis, GamepadButton, GamepadEvent, InputError, InputEvent, LockState};

const TAG_KEY: u8 = 0;
const TAG_POINTER_MOTION: u8 = 1;
const TAG_POINTER_ABSOLUTE: u8 = 2;
const TAG_POINTER_BUTTON: u8 = 3;
const TAG_SCROLL: u8 = 4;
const TAG_GAMEPAD: u8 = 5;
const TAG_LOCKS: u8 = 6;

const PAD_CONNECTED: u8 = 0;
const PAD_DISCONNECTED: u8 = 1;
const PAD_BUTTON: u8 = 2;
const PAD_AXIS: u8 = 3;

const TAG_LEN: usize = 1;
const BOOL_LEN: usize = 1;
const U32_LEN: usize = 4;
const F64_LEN: usize = 8;
const GAMEPAD_HEADER_LEN: usize = 2;
const PAD_BUTTON_LEN: usize = 2 + BOOL_LEN;
const PAD_AXIS_LEN: usize = 1 + 4;
const LOCKS_LEN: usize = 1;

impl InputEvent {
    pub fn encoded_len(&self) -> usize {
        TAG_LEN
            + match self {
                Self::Key { .. } | Self::PointerButton { .. } => U32_LEN + BOOL_LEN,
                Self::PointerMotion { .. } | Self::PointerAbsolute { .. } | Self::Scroll { .. } => {
                    2 * F64_LEN
                }
                Self::Gamepad { event, .. } => GAMEPAD_HEADER_LEN + gamepad_payload_len(*event),
                Self::Locks(_) => LOCKS_LEN,
            }
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.reserve(self.encoded_len());
        match *self {
            Self::Key { keycode, pressed } => {
                out.push(TAG_KEY);
                out.extend_from_slice(&keycode.to_le_bytes());
                out.push(u8::from(pressed));
            }
            Self::PointerMotion { dx, dy } => {
                out.push(TAG_POINTER_MOTION);
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Self::PointerAbsolute { x, y } => {
                out.push(TAG_POINTER_ABSOLUTE);
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            Self::PointerButton { button, pressed } => {
                out.push(TAG_POINTER_BUTTON);
                out.extend_from_slice(&button.to_le_bytes());
                out.push(u8::from(pressed));
            }
            Self::Scroll { dx, dy } => {
                out.push(TAG_SCROLL);
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Self::Gamepad { slot, event } => {
                out.push(TAG_GAMEPAD);
                out.push(slot);
                encode_gamepad(event, out);
            }
            Self::Locks(locks) => {
                out.push(TAG_LOCKS);
                out.push(locks.to_bits());
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, InputError> {
        let mut reader = Reader::new(bytes);

        let event = match reader.u8()? {
            TAG_KEY => Self::Key {
                keycode: reader.u32()?,
                pressed: reader.bool()?,
            },
            TAG_POINTER_MOTION => Self::PointerMotion {
                dx: reader.f64()?,
                dy: reader.f64()?,
            },
            TAG_POINTER_ABSOLUTE => Self::PointerAbsolute {
                x: reader.f64()?,
                y: reader.f64()?,
            },
            TAG_POINTER_BUTTON => Self::PointerButton {
                button: reader.u32()?,
                pressed: reader.bool()?,
            },
            TAG_SCROLL => Self::Scroll {
                dx: reader.f64()?,
                dy: reader.f64()?,
            },
            TAG_GAMEPAD => Self::Gamepad {
                slot: reader.u8()?,
                event: decode_gamepad(&mut reader)?,
            },
            TAG_LOCKS => Self::Locks(LockState::from_bits(reader.u8()?)?),
            tag => return Err(InputError::WireUnknownTag(tag)),
        };

        reader.finish()?;

        Ok(event)
    }
}

fn gamepad_payload_len(event: GamepadEvent) -> usize {
    match event {
        GamepadEvent::Connected | GamepadEvent::Disconnected => 0,
        GamepadEvent::Button { .. } => PAD_BUTTON_LEN,
        GamepadEvent::Axis { .. } => PAD_AXIS_LEN,
    }
}

fn encode_gamepad(event: GamepadEvent, out: &mut Vec<u8>) {
    match event {
        GamepadEvent::Connected => out.push(PAD_CONNECTED),
        GamepadEvent::Disconnected => out.push(PAD_DISCONNECTED),
        GamepadEvent::Button { button, pressed } => {
            out.push(PAD_BUTTON);
            out.extend_from_slice(&button.0.to_le_bytes());
            out.push(u8::from(pressed));
        }
        GamepadEvent::Axis { axis, value } => {
            out.push(PAD_AXIS);
            out.push(axis.0);
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
}

fn decode_gamepad(reader: &mut Reader) -> Result<GamepadEvent, InputError> {
    match reader.u8()? {
        PAD_CONNECTED => Ok(GamepadEvent::Connected),
        PAD_DISCONNECTED => Ok(GamepadEvent::Disconnected),
        PAD_BUTTON => Ok(GamepadEvent::Button {
            button: GamepadButton(reader.u16()?),
            pressed: reader.bool()?,
        }),
        PAD_AXIS => Ok(GamepadEvent::Axis {
            axis: GamepadAxis(reader.u8()?),
            value: reader.f32()?,
        }),
        tag => Err(InputError::WireUnknownGamepadTag(tag)),
    }
}

pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(rest: &'a [u8]) -> Self {
        Self { rest }
    }

    pub(crate) fn finish(&self) -> Result<(), InputError> {
        match self.rest.len() {
            0 => Ok(()),
            extra => Err(InputError::WireTrailingBytes(extra)),
        }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], InputError> {
        let Some((head, rest)) = self.rest.split_first_chunk::<N>() else {
            return Err(InputError::WireTruncated {
                missing: N - self.rest.len(),
            });
        };

        self.rest = rest;
        Ok(*head)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, InputError> {
        self.take::<1>().map(|[byte]| byte)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, InputError> {
        self.take().map(u16::from_le_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, InputError> {
        self.take().map(u32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, InputError> {
        self.take().map(u64::from_le_bytes)
    }

    pub(crate) fn f32(&mut self) -> Result<f32, InputError> {
        self.take().map(f32::from_le_bytes)
    }

    pub(crate) fn f64(&mut self) -> Result<f64, InputError> {
        self.take().map(f64::from_le_bytes)
    }

    pub(crate) fn bool(&mut self) -> Result<bool, InputError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(InputError::WireInvalidBool(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(event: InputEvent) -> Vec<u8> {
        let mut bytes = Vec::new();
        event.encode_into(&mut bytes);

        assert_eq!(bytes.len(), event.encoded_len());
        assert_eq!(InputEvent::decode(&bytes).unwrap(), event);
        bytes
    }

    fn samples() -> Vec<InputEvent> {
        vec![
            InputEvent::Key {
                keycode: 30,
                pressed: true,
            },
            InputEvent::Key {
                keycode: u32::MAX,
                pressed: false,
            },
            InputEvent::PointerMotion {
                dx: -3.5,
                dy: 12.25,
            },
            InputEvent::PointerAbsolute { x: 0.25, y: 0.999 },
            InputEvent::PointerButton {
                button: 0x110,
                pressed: true,
            },
            InputEvent::Scroll { dx: 0.0, dy: -1.5 },
            InputEvent::Gamepad {
                slot: 3,
                event: GamepadEvent::Connected,
            },
            InputEvent::Gamepad {
                slot: 0,
                event: GamepadEvent::Disconnected,
            },
            InputEvent::Gamepad {
                slot: 1,
                event: GamepadEvent::Button {
                    button: GamepadButton::START,
                    pressed: true,
                },
            },
            InputEvent::Gamepad {
                slot: 2,
                event: GamepadEvent::Axis {
                    axis: GamepadAxis::RIGHT_TRIGGER,
                    value: -0.75,
                },
            },
            InputEvent::Locks(LockState {
                caps: true,
                num: false,
            }),
        ]
    }

    #[test]
    fn every_event_round_trips() {
        for event in samples() {
            round_trip(event);
        }
    }

    #[test]
    fn encoding_is_little_endian_with_a_leading_tag() {
        let bytes = round_trip(InputEvent::Key {
            keycode: 0x0102_0304,
            pressed: true,
        });

        assert_eq!(bytes, [TAG_KEY, 4, 3, 2, 1, 1]);
    }

    #[test]
    fn non_finite_floats_survive_the_wire_bit_for_bit() {
        let mut bytes = Vec::new();
        InputEvent::PointerMotion {
            dx: f64::NAN,
            dy: f64::NEG_INFINITY,
        }
        .encode_into(&mut bytes);

        let decoded = InputEvent::decode(&bytes).unwrap();

        let InputEvent::PointerMotion { dx, dy } = decoded else {
            panic!("decoded {decoded:?}");
        };
        assert!(dx.is_nan());
        assert_eq!(dy, f64::NEG_INFINITY);
    }

    #[test]
    fn every_strict_prefix_is_truncated() {
        for event in samples() {
            let mut bytes = Vec::new();
            event.encode_into(&mut bytes);

            for length in 0..bytes.len() {
                let result = InputEvent::decode(&bytes[..length]);

                assert!(
                    matches!(result, Err(InputError::WireTruncated { missing }) if missing > 0),
                    "{event:?} cut to {length} bytes gave {result:?}"
                );
            }
        }
    }

    #[test]
    fn missing_bytes_are_counted() {
        let result = InputEvent::decode(&[TAG_KEY, 1, 2]);

        assert!(matches!(
            result,
            Err(InputError::WireTruncated { missing: 2 })
        ));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = round_trip(InputEvent::Scroll { dx: 1.0, dy: 2.0 });
        bytes.extend_from_slice(&[0, 0, 0]);

        let result = InputEvent::decode(&bytes);

        assert!(matches!(result, Err(InputError::WireTrailingBytes(3))));
    }

    #[test]
    fn unknown_tags_are_rejected() {
        let event = InputEvent::decode(&[200]);
        let pad = InputEvent::decode(&[TAG_GAMEPAD, 0, 77]);

        assert!(matches!(event, Err(InputError::WireUnknownTag(200))));
        assert!(matches!(pad, Err(InputError::WireUnknownGamepadTag(77))));
    }

    #[test]
    fn booleans_other_than_zero_and_one_are_rejected() {
        let result = InputEvent::decode(&[TAG_KEY, 0, 0, 0, 0, 2]);

        assert!(matches!(result, Err(InputError::WireInvalidBool(2))));
    }

    #[test]
    fn lock_state_uses_one_byte_and_rejects_unknown_bits() {
        let bytes = round_trip(InputEvent::Locks(LockState {
            caps: true,
            num: true,
        }));
        let unknown = InputEvent::decode(&[TAG_LOCKS, 4]);

        assert_eq!(bytes, [TAG_LOCKS, 3]);
        assert!(matches!(unknown, Err(InputError::WireInvalidLockBits(4))));
    }

    #[test]
    fn empty_input_is_truncated() {
        let result = InputEvent::decode(&[]);

        assert!(matches!(
            result,
            Err(InputError::WireTruncated { missing: 1 })
        ));
    }
}
