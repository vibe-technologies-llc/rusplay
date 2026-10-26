use crate::{GamepadAxis, GamepadButton, GamepadEvent, InputEvent};

pub const MAX_PADS: usize = 16;
pub const PAD_AXES: usize = 6;

const KEY_SET_WORDS: usize = 12;
const KEY_SET_WORDS_U32: u32 = 12;
const BITS_PER_WORD: u32 = u64::BITS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeySet([u64; KEY_SET_WORDS]);

impl KeySet {
    pub const CAPACITY: u32 = BITS_PER_WORD * KEY_SET_WORDS_U32;
    pub const WORDS: usize = KEY_SET_WORDS;

    pub const fn from_words(words: [u64; KEY_SET_WORDS]) -> Self {
        Self(words)
    }

    pub const fn words(&self) -> &[u64; KEY_SET_WORDS] {
        &self.0
    }

    pub fn insert(&mut self, code: u32) -> bool {
        match locate(code) {
            Some((word, bit)) => {
                self.0[word] |= bit;
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, code: u32) {
        if let Some((word, bit)) = locate(code) {
            self.0[word] &= !bit;
        }
    }

    pub fn contains(&self, code: u32) -> bool {
        locate(code).is_some_and(|(word, bit)| self.0[word] & bit != 0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|word| *word == 0)
    }

    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        (0..Self::CAPACITY).filter(|code| self.contains(*code))
    }

    fn set(&mut self, code: u32, held: bool) {
        if held {
            self.insert(code);
        } else {
            self.remove(code);
        }
    }
}

fn locate(code: u32) -> Option<(usize, u64)> {
    let word = usize::try_from(code / BITS_PER_WORD).ok()?;

    (word < KEY_SET_WORDS).then(|| (word, 1u64 << (code % BITS_PER_WORD)))
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PadState {
    pub buttons: u16,
    pub axes: [f32; PAD_AXES],
}

impl PadState {
    pub fn button_held(&self, button: GamepadButton) -> bool {
        u32::from(button.0) < u16::BITS && self.buttons & (1 << button.0) != 0
    }

    fn set_button(&mut self, button: GamepadButton, held: bool) {
        if u32::from(button.0) >= u16::BITS {
            return;
        }

        let bit = 1u16 << button.0;
        if held {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
    }

    fn set_axis(&mut self, axis: GamepadAxis, value: f32) {
        if let Some(slot) = self.axes.get_mut(usize::from(axis.0))
            && value.is_finite()
        {
            *slot = value;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeldInput {
    keys: KeySet,
    buttons: KeySet,
    pads: [Option<PadState>; MAX_PADS],
}

impl HeldInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_idle(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty() && self.pads.iter().all(Option::is_none)
    }

    pub const fn keys(&self) -> &KeySet {
        &self.keys
    }

    pub const fn buttons(&self) -> &KeySet {
        &self.buttons
    }

    pub fn pad(&self, slot: u8) -> Option<&PadState> {
        self.pads.get(usize::from(slot))?.as_ref()
    }

    pub fn observe(&mut self, event: &InputEvent) {
        match *event {
            InputEvent::Key { keycode, pressed } => self.keys.set(keycode, pressed),
            InputEvent::PointerButton { button, pressed } => self.buttons.set(button, pressed),
            InputEvent::Gamepad { slot, event } => self.observe_pad(slot, event),
            InputEvent::PointerMotion { .. }
            | InputEvent::PointerAbsolute { .. }
            | InputEvent::Scroll { .. } => {}
        }
    }

    fn observe_pad(&mut self, slot: u8, event: GamepadEvent) {
        let Some(entry) = self.pads.get_mut(usize::from(slot)) else {
            return;
        };

        match event {
            GamepadEvent::Connected => *entry = Some(PadState::default()),
            GamepadEvent::Disconnected => *entry = None,
            GamepadEvent::Button { button, pressed } => {
                if let Some(pad) = entry {
                    pad.set_button(button, pressed);
                }
            }
            GamepadEvent::Axis { axis, value } => {
                if let Some(pad) = entry {
                    pad.set_axis(axis, value);
                }
            }
        }
    }

    pub fn release_all(&self, out: &mut Vec<InputEvent>) {
        out.extend(self.keys.iter().map(|keycode| InputEvent::Key {
            keycode,
            pressed: false,
        }));
        out.extend(self.buttons.iter().map(|button| InputEvent::PointerButton {
            button,
            pressed: false,
        }));

        for (slot, pad) in (0u8..).zip(&self.pads) {
            if pad.is_some() {
                out.push(pad_event(slot, GamepadEvent::Disconnected));
            }
        }
    }
}

fn pad_event(slot: u8, event: GamepadEvent) -> InputEvent {
    InputEvent::Gamepad { slot, event }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(keycode: u32, pressed: bool) -> InputEvent {
        InputEvent::Key { keycode, pressed }
    }

    fn click(button: u32, pressed: bool) -> InputEvent {
        InputEvent::PointerButton { button, pressed }
    }

    fn pad(slot: u8, event: GamepadEvent) -> InputEvent {
        InputEvent::Gamepad { slot, event }
    }

    fn press(slot: u8, button: GamepadButton) -> InputEvent {
        pad(
            slot,
            GamepadEvent::Button {
                button,
                pressed: true,
            },
        )
    }

    fn axis(slot: u8, axis: GamepadAxis, value: f32) -> InputEvent {
        pad(slot, GamepadEvent::Axis { axis, value })
    }

    fn held(events: &[InputEvent]) -> HeldInput {
        let mut held = HeldInput::new();
        for event in events {
            held.observe(event);
        }
        held
    }

    #[test]
    fn key_set_holds_every_evdev_code_and_refuses_the_rest() {
        let mut keys = KeySet::default();

        assert!(keys.insert(0));
        assert!(keys.insert(30));
        assert!(keys.insert(0x2ff));
        assert!(!keys.insert(KeySet::CAPACITY));
        keys.remove(30);

        assert_eq!(keys.iter().collect::<Vec<_>>(), vec![0, 0x2ff]);
        assert!(!keys.contains(30));
        assert!(!keys.contains(u32::MAX));
    }

    #[test]
    fn observing_events_tracks_keys_buttons_and_pads() {
        let state = held(&[
            key(30, true),
            key(31, true),
            key(30, false),
            click(0x110, true),
            pad(1, GamepadEvent::Connected),
            press(1, GamepadButton::SOUTH),
            axis(1, GamepadAxis::LEFT_X, 0.5),
            InputEvent::PointerMotion { dx: 1.0, dy: 2.0 },
        ]);

        assert_eq!(state.keys().iter().collect::<Vec<_>>(), vec![31]);
        assert_eq!(state.buttons().iter().collect::<Vec<_>>(), vec![0x110]);
        assert!(state.pad(1).unwrap().button_held(GamepadButton::SOUTH));
        assert_eq!(state.pad(1).unwrap().axes[0], 0.5);
        assert!(state.pad(0).is_none());
        assert!(!state.is_idle());
    }

    #[test]
    fn a_pad_event_for_a_slot_that_is_not_connected_is_ignored() {
        let state = held(&[
            press(2, GamepadButton::EAST),
            axis(2, GamepadAxis::RIGHT_Y, 1.0),
            pad(40, GamepadEvent::Connected),
        ]);

        assert!(state.is_idle());
    }

    #[test]
    fn disconnecting_a_pad_forgets_its_state() {
        let state = held(&[
            pad(0, GamepadEvent::Connected),
            press(0, GamepadButton::START),
            pad(0, GamepadEvent::Disconnected),
            pad(0, GamepadEvent::Connected),
        ]);

        assert_eq!(state.pad(0), Some(&PadState::default()));
    }

    #[test]
    fn non_finite_axis_values_and_unknown_ids_are_not_tracked() {
        let state = held(&[
            pad(0, GamepadEvent::Connected),
            axis(0, GamepadAxis::LEFT_X, f32::NAN),
            axis(0, GamepadAxis(200), 1.0),
            press(0, GamepadButton(99)),
        ]);

        assert_eq!(state.pad(0), Some(&PadState::default()));
    }

    #[test]
    fn releasing_everything_lifts_keys_buttons_and_pads() {
        let state = held(&[
            key(30, true),
            click(0x111, true),
            pad(1, GamepadEvent::Connected),
            press(1, GamepadButton::SOUTH),
        ]);
        let mut events = Vec::new();

        state.release_all(&mut events);
        let mut after = state.clone();
        for event in &events {
            after.observe(event);
        }

        assert_eq!(
            events,
            vec![
                key(30, false),
                click(0x111, false),
                pad(1, GamepadEvent::Disconnected)
            ]
        );
        assert!(after.is_idle());
    }
}
