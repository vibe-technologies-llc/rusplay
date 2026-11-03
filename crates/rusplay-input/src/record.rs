use crate::{
    InputError, InputEvent, LockState, SensorKind,
    held::{KeySet, MAX_PAD_FINGERS, MAX_PADS, PAD_AXES, PadFingers, PadState, TouchSet},
    wire::{Reader, decode_sensor_kind, sensor_kind_byte},
};

const TAG_KEYS: u8 = 16;
const TAG_BUTTONS: u8 = 17;
const TAG_PADS: u8 = 18;
const TAG_PAD: u8 = 19;
const TAG_LOCKS: u8 = 20;
const TAG_TOUCHES: u8 = 21;
const TAG_PAD_SENSOR: u8 = 22;
const TAG_PAD_TOUCHPAD: u8 = 23;
const LAST_STATE_TAG: u8 = TAG_PAD_TOUCHPAD;

const TAG_LEN: usize = 1;
const KEY_SET_LEN: usize = KeySet::WORDS * size_of::<u64>();
const PADS_LEN: usize = size_of::<u16>();
const LOCKS_LEN: usize = 1;
const TOUCHES_LEN: usize = size_of::<u16>();
const F32_LEN: usize = size_of::<f32>();
const PAD_SENSOR_LEN: usize = 1 + 1 + 3 * F32_LEN;
const PAD_TOUCHPAD_HEADER_LEN: usize = 1 + 1;
const FINGER_LEN: usize = 2 * F32_LEN;
const PAD_LEN: usize = 1 + size_of::<u16>() + PAD_AXES * size_of::<f32>();

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StateRecord {
    Keys(KeySet),
    Buttons(KeySet),
    Pads(u16),
    Pad {
        slot: u8,
        state: PadState,
    },
    PadSensor {
        slot: u8,
        sensor: SensorKind,
        values: [f32; 3],
    },
    PadTouchpad {
        slot: u8,
        fingers: PadFingers,
    },
    Locks(LockState),
    Touches(TouchSet),
}

impl StateRecord {
    pub const MAX_ENCODED_LEN: usize = TAG_LEN + KEY_SET_LEN;

    pub const fn encoded_len(&self) -> usize {
        TAG_LEN
            + match self {
                Self::Keys(_) | Self::Buttons(_) => KEY_SET_LEN,
                Self::Pads(_) => PADS_LEN,
                Self::Pad { .. } => PAD_LEN,
                Self::PadSensor { .. } => PAD_SENSOR_LEN,
                Self::PadTouchpad { fingers, .. } => {
                    PAD_TOUCHPAD_HEADER_LEN + finger_count(fingers) * FINGER_LEN
                }
                Self::Locks(_) => LOCKS_LEN,
                Self::Touches(_) => TOUCHES_LEN,
            }
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.reserve(self.encoded_len());
        match self {
            Self::Keys(keys) => {
                out.push(TAG_KEYS);
                encode_key_set(keys, out);
            }
            Self::Buttons(buttons) => {
                out.push(TAG_BUTTONS);
                encode_key_set(buttons, out);
            }
            Self::Pads(mask) => {
                out.push(TAG_PADS);
                out.extend_from_slice(&mask.to_le_bytes());
            }
            Self::Pad { slot, state } => {
                out.push(TAG_PAD);
                out.push(*slot);
                out.extend_from_slice(&state.buttons.to_le_bytes());
                for axis in state.axes {
                    out.extend_from_slice(&axis.to_le_bytes());
                }
            }
            Self::PadSensor {
                slot,
                sensor,
                values,
            } => {
                out.push(TAG_PAD_SENSOR);
                out.push(*slot);
                out.push(sensor_kind_byte(*sensor));
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            Self::PadTouchpad { slot, fingers } => {
                out.push(TAG_PAD_TOUCHPAD);
                out.push(*slot);
                out.push(finger_mask(fingers));
                for (x, y) in fingers.iter().flatten() {
                    out.extend_from_slice(&x.to_le_bytes());
                    out.extend_from_slice(&y.to_le_bytes());
                }
            }
            Self::Locks(locks) => {
                out.push(TAG_LOCKS);
                out.push(locks.to_bits());
            }
            Self::Touches(touches) => {
                out.push(TAG_TOUCHES);
                out.extend_from_slice(&touches.bits().to_le_bytes());
            }
        }
    }

    fn decode_body(tag: u8, reader: &mut Reader<'_>) -> Result<Self, InputError> {
        match tag {
            TAG_KEYS => Ok(Self::Keys(decode_key_set(reader)?)),
            TAG_BUTTONS => Ok(Self::Buttons(decode_key_set(reader)?)),
            TAG_PADS => Ok(Self::Pads(reader.u16()?)),
            TAG_PAD => {
                let slot = pad_slot(reader.u8()?)?;

                let mut state = PadState {
                    buttons: reader.u16()?,
                    ..PadState::default()
                };
                for axis in &mut state.axes {
                    *axis = finite(reader.f32()?)?;
                }

                Ok(Self::Pad { slot, state })
            }
            TAG_PAD_SENSOR => {
                let slot = pad_slot(reader.u8()?)?;
                let sensor = decode_sensor_kind(reader.u8()?)?;
                let values = [
                    finite(reader.f32()?)?,
                    finite(reader.f32()?)?,
                    finite(reader.f32()?)?,
                ];

                Ok(Self::PadSensor {
                    slot,
                    sensor,
                    values,
                })
            }
            TAG_PAD_TOUCHPAD => {
                let slot = pad_slot(reader.u8()?)?;
                let mask = reader.u8()?;
                if u32::from(mask) >> MAX_PAD_FINGERS != 0 {
                    return Err(InputError::PadFingerOutOfRange(highest_bit(mask)));
                }

                let mut fingers = PadFingers::default();
                for (index, finger) in fingers.iter_mut().enumerate() {
                    if mask & (1 << index) != 0 {
                        *finger = Some((finite(reader.f32()?)?, finite(reader.f32()?)?));
                    }
                }

                Ok(Self::PadTouchpad { slot, fingers })
            }
            TAG_TOUCHES => Ok(Self::Touches(TouchSet::from_bits(reader.u16()?))),
            TAG_LOCKS => Ok(Self::Locks(LockState::from_bits(reader.u8()?)?)),
            tag => Err(InputError::WireUnknownTag(tag)),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, InputError> {
        let mut reader = Reader::new(bytes);
        let tag = reader.u8()?;
        let record = Self::decode_body(tag, &mut reader)?;
        reader.finish()?;

        Ok(record)
    }
}

fn pad_slot(slot: u8) -> Result<u8, InputError> {
    if usize::from(slot) >= MAX_PADS {
        return Err(InputError::GamepadSlotOutOfRange(slot));
    }

    Ok(slot)
}

fn finite(value: f32) -> Result<f32, InputError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(InputError::NonFiniteValue)
    }
}

const fn finger_count(fingers: &PadFingers) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < MAX_PAD_FINGERS {
        if fingers[index].is_some() {
            count += 1;
        }
        index += 1;
    }

    count
}

fn finger_mask(fingers: &PadFingers) -> u8 {
    (0..MAX_PAD_FINGERS)
        .filter(|index| fingers[*index].is_some())
        .fold(0, |mask, index| mask | (1 << index))
}

fn highest_bit(mask: u8) -> u8 {
    u8::try_from(u8::BITS - 1 - mask.leading_zeros()).unwrap_or(u8::MAX)
}

fn encode_key_set(keys: &KeySet, out: &mut Vec<u8>) {
    for word in keys.words() {
        out.extend_from_slice(&word.to_le_bytes());
    }
}

fn decode_key_set(reader: &mut Reader<'_>) -> Result<KeySet, InputError> {
    let mut words = [0u64; KeySet::WORDS];
    for word in &mut words {
        *word = reader.u64()?;
    }

    Ok(KeySet::from_words(words))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputRecord {
    Event(InputEvent),
    State(StateRecord),
}

impl InputRecord {
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Event(event) => event.encoded_len(),
            Self::State(state) => state.encoded_len(),
        }
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Event(event) => event.encode_into(out),
            Self::State(state) => state.encode_into(out),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, InputError> {
        match bytes.first() {
            Some(&(TAG_KEYS..=LAST_STATE_TAG)) => StateRecord::decode(bytes).map(Self::State),
            _ => InputEvent::decode(bytes).map(Self::Event),
        }
    }
}

impl From<InputEvent> for InputRecord {
    fn from(event: InputEvent) -> Self {
        Self::Event(event)
    }
}

impl From<StateRecord> for InputRecord {
    fn from(state: StateRecord) -> Self {
        Self::State(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GamepadEvent;

    fn round_trip(record: StateRecord) -> Vec<u8> {
        let mut bytes = Vec::new();
        record.encode_into(&mut bytes);

        assert_eq!(bytes.len(), record.encoded_len());
        assert_eq!(StateRecord::decode(&bytes).unwrap(), record);
        assert_eq!(
            InputRecord::decode(&bytes).unwrap(),
            InputRecord::State(record)
        );
        bytes
    }

    fn keys(codes: &[u32]) -> KeySet {
        let mut set = KeySet::default();
        for code in codes {
            set.insert(*code);
        }
        set
    }

    #[test]
    fn state_records_round_trip() {
        round_trip(StateRecord::Keys(keys(&[0, 30, 63, 64, 0x2ff])));
        round_trip(StateRecord::Buttons(keys(&[0x110, 0x111])));
        round_trip(StateRecord::Keys(KeySet::default()));
        round_trip(StateRecord::Pads(0b1010_0000_0000_0101));
        round_trip(StateRecord::Touches(TouchSet::from_bits(
            0b1000_0000_0000_0110,
        )));
        round_trip(StateRecord::Locks(LockState {
            caps: false,
            num: true,
        }));
        round_trip(StateRecord::Pad {
            slot: 15,
            state: PadState {
                buttons: 0x8001,
                axes: [-1.0, 0.25, 0.0, 1.0, 0.5, 0.125],
                ..PadState::default()
            },
        });
        round_trip(StateRecord::PadSensor {
            slot: 3,
            sensor: SensorKind::Gyro,
            values: [-1.5, 0.0, 9.5],
        });
        round_trip(StateRecord::PadSensor {
            slot: 0,
            sensor: SensorKind::Accel,
            values: [0.0, -9.80665, 0.0],
        });
        round_trip(StateRecord::PadTouchpad {
            slot: 2,
            fingers: PadFingers::default(),
        });
        round_trip(StateRecord::PadTouchpad {
            slot: 2,
            fingers: [Some((0.25, 0.5)), None, Some((1.0, 0.0)), None],
        });
    }

    #[test]
    fn the_longest_record_fits_the_one_byte_record_length() {
        let longest = StateRecord::Keys(KeySet::default()).encoded_len();

        assert_eq!(longest, StateRecord::MAX_ENCODED_LEN);
        assert!(u8::try_from(longest).is_ok());
    }

    #[test]
    fn touchpad_records_are_sized_by_their_fingers() {
        let two = StateRecord::PadTouchpad {
            slot: 1,
            fingers: [None, Some((0.1, 0.2)), None, Some((0.3, 0.4))],
        };
        let mut bytes = Vec::new();
        two.encode_into(&mut bytes);

        assert_eq!(bytes.len(), 3 + 2 * 8);
        assert_eq!(&bytes[..3], &[TAG_PAD_TOUCHPAD, 1, 0b1010]);
    }

    #[test]
    fn malformed_sensor_and_touchpad_records_are_rejected() {
        let mut sensor = Vec::new();
        StateRecord::PadSensor {
            slot: 1,
            sensor: SensorKind::Gyro,
            values: [0.0; 3],
        }
        .encode_into(&mut sensor);

        let mut bad_slot = sensor.clone();
        bad_slot[1] = 16;
        let mut bad_kind = sensor.clone();
        bad_kind[2] = 9;
        let mut not_finite = sensor.clone();
        not_finite[7..11].copy_from_slice(&f32::INFINITY.to_le_bytes());

        let mut touchpad = vec![TAG_PAD_TOUCHPAD, 0, 0b0001_0000];
        touchpad.extend_from_slice(&[0; 8]);
        let mut touchpad_nan = vec![TAG_PAD_TOUCHPAD, 0, 0b0001];
        touchpad_nan.extend_from_slice(&f32::NAN.to_le_bytes());
        touchpad_nan.extend_from_slice(&[0; 4]);
        let mut touchpad_cut = vec![TAG_PAD_TOUCHPAD, 0, 0b0011];
        touchpad_cut.extend_from_slice(&[0; 8]);

        assert!(matches!(
            StateRecord::decode(&bad_slot),
            Err(InputError::GamepadSlotOutOfRange(16))
        ));
        assert!(matches!(
            StateRecord::decode(&bad_kind),
            Err(InputError::WireUnknownSensorKind(9))
        ));
        assert!(matches!(
            StateRecord::decode(&not_finite),
            Err(InputError::NonFiniteValue)
        ));
        assert!(matches!(
            StateRecord::decode(&touchpad),
            Err(InputError::PadFingerOutOfRange(4))
        ));
        assert!(matches!(
            StateRecord::decode(&touchpad_nan),
            Err(InputError::NonFiniteValue)
        ));
        assert!(matches!(
            StateRecord::decode(&touchpad_cut),
            Err(InputError::WireTruncated { .. })
        ));
        assert!(matches!(
            StateRecord::decode(&[TAG_PAD_TOUCHPAD, 0, 0xff]),
            Err(InputError::PadFingerOutOfRange(7))
        ));
    }

    #[test]
    fn every_strict_prefix_of_a_pad_record_is_truncated() {
        let records = [
            StateRecord::PadSensor {
                slot: 1,
                sensor: SensorKind::Accel,
                values: [1.0, 2.0, 3.0],
            },
            StateRecord::PadTouchpad {
                slot: 1,
                fingers: [Some((0.5, 0.5)), Some((0.25, 0.75)), None, None],
            },
        ];

        for record in records {
            let mut bytes = Vec::new();
            record.encode_into(&mut bytes);

            for length in 0..bytes.len() {
                assert!(matches!(
                    StateRecord::decode(&bytes[..length]),
                    Err(InputError::WireTruncated { .. })
                ));
            }
        }
    }

    #[test]
    fn events_decode_through_the_record_decoder() {
        let event = InputEvent::Gamepad {
            slot: 2,
            event: GamepadEvent::Connected,
        };
        let mut bytes = Vec::new();
        InputRecord::Event(event).encode_into(&mut bytes);

        assert_eq!(bytes.len(), InputRecord::Event(event).encoded_len());
        assert_eq!(
            InputRecord::decode(&bytes).unwrap(),
            InputRecord::Event(event)
        );
    }

    #[test]
    fn malformed_state_records_are_rejected_with_their_reason() {
        let mut pad = Vec::new();
        StateRecord::Pad {
            slot: 1,
            state: PadState::default(),
        }
        .encode_into(&mut pad);

        let mut bad_slot = pad.clone();
        bad_slot[1] = 16;
        let mut not_finite = pad.clone();
        not_finite[4..8].copy_from_slice(&f32::NAN.to_le_bytes());
        let mut trailing = pad.clone();
        trailing.push(0);

        assert!(matches!(
            StateRecord::decode(&bad_slot),
            Err(InputError::GamepadSlotOutOfRange(16))
        ));
        assert!(matches!(
            StateRecord::decode(&not_finite),
            Err(InputError::NonFiniteValue)
        ));
        assert!(matches!(
            StateRecord::decode(&trailing),
            Err(InputError::WireTrailingBytes(1))
        ));
        assert!(matches!(
            StateRecord::decode(&pad[..pad.len() - 1]),
            Err(InputError::WireTruncated { .. })
        ));
        assert!(matches!(
            InputRecord::decode(&[LAST_STATE_TAG + 1]),
            Err(InputError::WireUnknownTag(_))
        ));
        assert!(matches!(
            InputRecord::decode(&[]),
            Err(InputError::WireTruncated { .. })
        ));
    }
}
