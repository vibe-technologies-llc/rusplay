use crate::{
    InputError, InputEvent, LockState,
    held::{KeySet, MAX_PADS, PAD_AXES, PadState},
    wire::Reader,
};

const TAG_KEYS: u8 = 16;
const TAG_BUTTONS: u8 = 17;
const TAG_PADS: u8 = 18;
const TAG_PAD: u8 = 19;
const TAG_LOCKS: u8 = 20;
const LAST_STATE_TAG: u8 = TAG_LOCKS;

const TAG_LEN: usize = 1;
const KEY_SET_LEN: usize = KeySet::WORDS * size_of::<u64>();
const PADS_LEN: usize = size_of::<u16>();
const LOCKS_LEN: usize = 1;
const PAD_LEN: usize = 1 + size_of::<u16>() + PAD_AXES * size_of::<f32>();

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StateRecord {
    Keys(KeySet),
    Buttons(KeySet),
    Pads(u16),
    Pad { slot: u8, state: PadState },
    Locks(LockState),
}

impl StateRecord {
    pub const MAX_ENCODED_LEN: usize = TAG_LEN + KEY_SET_LEN;

    pub const fn encoded_len(&self) -> usize {
        TAG_LEN
            + match self {
                Self::Keys(_) | Self::Buttons(_) => KEY_SET_LEN,
                Self::Pads(_) => PADS_LEN,
                Self::Pad { .. } => PAD_LEN,
                Self::Locks(_) => LOCKS_LEN,
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
            Self::Locks(locks) => {
                out.push(TAG_LOCKS);
                out.push(locks.to_bits());
            }
        }
    }

    fn decode_body(tag: u8, reader: &mut Reader<'_>) -> Result<Self, InputError> {
        match tag {
            TAG_KEYS => Ok(Self::Keys(decode_key_set(reader)?)),
            TAG_BUTTONS => Ok(Self::Buttons(decode_key_set(reader)?)),
            TAG_PADS => Ok(Self::Pads(reader.u16()?)),
            TAG_PAD => {
                let slot = reader.u8()?;
                if usize::from(slot) >= MAX_PADS {
                    return Err(InputError::GamepadSlotOutOfRange(slot));
                }

                let mut state = PadState {
                    buttons: reader.u16()?,
                    ..PadState::default()
                };
                for axis in &mut state.axes {
                    *axis = reader.f32()?;
                    if !axis.is_finite() {
                        return Err(InputError::NonFiniteValue);
                    }
                }

                Ok(Self::Pad { slot, state })
            }
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
        round_trip(StateRecord::Locks(LockState {
            caps: false,
            num: true,
        }));
        round_trip(StateRecord::Pad {
            slot: 15,
            state: PadState {
                buttons: 0x8001,
                axes: [-1.0, 0.25, 0.0, 1.0, 0.5, 0.125],
            },
        });
    }

    #[test]
    fn the_longest_record_fits_the_one_byte_record_length() {
        let longest = StateRecord::Keys(KeySet::default()).encoded_len();

        assert_eq!(longest, StateRecord::MAX_ENCODED_LEN);
        assert!(u8::try_from(longest).is_ok());
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
