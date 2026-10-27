use crate::{
    GamepadAxis, GamepadButton, GamepadEvent, InputEvent, LockState, StateRecord, TouchEvent,
};

pub const MAX_PADS: usize = 16;
pub const PAD_AXES: usize = 6;
pub const MAX_TOUCHES: usize = 16;

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

    fn difference(&self, other: &Self) -> impl Iterator<Item = u32> + '_ {
        let other = *other;

        self.iter().filter(move |code| !other.contains(*code))
    }
}

fn locate(code: u32) -> Option<(usize, u64)> {
    let word = usize::try_from(code / BITS_PER_WORD).ok()?;

    (word < KEY_SET_WORDS).then(|| (word, 1u64 << (code % BITS_PER_WORD)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TouchSet(u16);

impl TouchSet {
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, slot: u8) -> bool {
        u32::from(slot) < u16::BITS && self.0 & (1 << slot) != 0
    }

    pub fn insert(&mut self, slot: u8) {
        if u32::from(slot) < u16::BITS {
            self.0 |= 1 << slot;
        }
    }

    pub fn remove(&mut self, slot: u8) {
        if u32::from(slot) < u16::BITS {
            self.0 &= !(1 << slot);
        }
    }

    pub fn iter(self) -> impl Iterator<Item = u8> {
        (0u8..)
            .take(MAX_TOUCHES)
            .filter(move |slot| self.contains(*slot))
    }
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
    locks: Option<LockState>,
    touches: TouchSet,
}

impl HeldInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_idle(&self) -> bool {
        self.keys.is_empty()
            && self.buttons.is_empty()
            && self.touches.is_empty()
            && self.pads.iter().all(Option::is_none)
    }

    pub const fn keys(&self) -> &KeySet {
        &self.keys
    }

    pub const fn buttons(&self) -> &KeySet {
        &self.buttons
    }

    pub const fn locks(&self) -> Option<LockState> {
        self.locks
    }

    pub const fn touches(&self) -> TouchSet {
        self.touches
    }

    pub fn pad(&self, slot: u8) -> Option<&PadState> {
        self.pads.get(usize::from(slot))?.as_ref()
    }

    pub fn observe(&mut self, event: &InputEvent) {
        match *event {
            InputEvent::Key { keycode, pressed } => self.keys.set(keycode, pressed),
            InputEvent::PointerButton { button, pressed } => self.buttons.set(button, pressed),
            InputEvent::Gamepad { slot, event } => self.observe_pad(slot, event),
            InputEvent::Locks(locks) => self.locks = Some(locks),
            InputEvent::Touch { slot, event } => match event {
                TouchEvent::Down { .. } => self.touches.insert(slot),
                TouchEvent::Up | TouchEvent::Cancel => self.touches.remove(slot),
                TouchEvent::Motion { .. } => {}
            },
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

    pub fn snapshot(&self, out: &mut Vec<StateRecord>) {
        out.push(StateRecord::Keys(self.keys));
        out.push(StateRecord::Buttons(self.buttons));
        out.push(StateRecord::Pads(self.connected_mask()));
        out.push(StateRecord::Touches(self.touches));
        out.extend(self.locks.map(StateRecord::Locks));

        for (slot, pad) in (0u8..).zip(&self.pads) {
            if let Some(state) = pad {
                out.push(StateRecord::Pad {
                    slot,
                    state: *state,
                });
            }
        }
    }

    pub fn reconcile(&self, record: &StateRecord, out: &mut Vec<InputEvent>) {
        match record {
            StateRecord::Keys(target) => diff_keys(&self.keys, target, out, |keycode, pressed| {
                InputEvent::Key { keycode, pressed }
            }),
            StateRecord::Buttons(target) => {
                diff_keys(&self.buttons, target, out, |button, pressed| {
                    InputEvent::PointerButton { button, pressed }
                });
            }
            StateRecord::Pads(mask) => self.reconcile_connections(*mask, out),
            StateRecord::Pad { slot, state } => self.reconcile_pad(*slot, state, out),
            StateRecord::Touches(target) => {
                out.extend(
                    self.touches
                        .iter()
                        .filter(|slot| !target.contains(*slot))
                        .map(|slot| InputEvent::Touch {
                            slot,
                            event: TouchEvent::Up,
                        }),
                );
            }
            StateRecord::Locks(target) => {
                if self.locks != Some(*target) {
                    out.push(InputEvent::Locks(*target));
                }
            }
        }
    }

    pub fn release_all(&self, out: &mut Vec<InputEvent>) {
        diff_keys(&self.keys, &KeySet::default(), out, |keycode, pressed| {
            InputEvent::Key { keycode, pressed }
        });
        diff_keys(&self.buttons, &KeySet::default(), out, |button, pressed| {
            InputEvent::PointerButton { button, pressed }
        });

        out.extend(self.touches.iter().map(|slot| InputEvent::Touch {
            slot,
            event: TouchEvent::Up,
        }));

        for (slot, pad) in (0u8..).zip(&self.pads) {
            if pad.is_some() {
                out.push(pad_event(slot, GamepadEvent::Disconnected));
            }
        }
    }

    fn connected_mask(&self) -> u16 {
        self.pads
            .iter()
            .enumerate()
            .filter(|(_, pad)| pad.is_some())
            .fold(0, |mask, (slot, _)| mask | (1 << slot))
    }

    fn reconcile_connections(&self, mask: u16, out: &mut Vec<InputEvent>) {
        let held = self.connected_mask();

        for slot in 0..MAX_PADS {
            let bit = 1u16 << slot;
            let Ok(slot) = u8::try_from(slot) else {
                continue;
            };

            if held & bit != 0 && mask & bit == 0 {
                out.push(pad_event(slot, GamepadEvent::Disconnected));
            }
            if held & bit == 0 && mask & bit != 0 {
                out.push(pad_event(slot, GamepadEvent::Connected));
            }
        }
    }

    fn reconcile_pad(&self, slot: u8, target: &PadState, out: &mut Vec<InputEvent>) {
        let Some(entry) = self.pads.get(usize::from(slot)) else {
            return;
        };

        if entry.is_none() {
            out.push(pad_event(slot, GamepadEvent::Connected));
        }
        let current = entry.unwrap_or_default();

        let changed = current.buttons ^ target.buttons;
        let released = changed & current.buttons;
        let pressed = changed & target.buttons;
        for (mask, held) in [(released, false), (pressed, true)] {
            for id in (0..u16::BITS).filter(|id| mask & (1 << id) != 0) {
                let Ok(id) = u16::try_from(id) else {
                    continue;
                };
                out.push(pad_event(
                    slot,
                    GamepadEvent::Button {
                        button: GamepadButton(id),
                        pressed: held,
                    },
                ));
            }
        }

        for (id, (have, want)) in (0u8..).zip(current.axes.iter().zip(&target.axes)) {
            if have.to_bits() != want.to_bits() {
                out.push(pad_event(
                    slot,
                    GamepadEvent::Axis {
                        axis: GamepadAxis(id),
                        value: *want,
                    },
                ));
            }
        }
    }
}

fn diff_keys(
    held: &KeySet,
    target: &KeySet,
    out: &mut Vec<InputEvent>,
    event: impl Fn(u32, bool) -> InputEvent,
) {
    out.extend(held.difference(target).map(|code| event(code, false)));
    out.extend(target.difference(held).map(|code| event(code, true)));
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

    fn converge(host: &mut HeldInput, client: &HeldInput) -> Vec<InputEvent> {
        let mut records = Vec::new();
        client.snapshot(&mut records);

        let mut applied = Vec::new();
        for record in &records {
            let mut events = Vec::new();
            host.reconcile(record, &mut events);
            for event in &events {
                host.observe(event);
            }
            applied.extend(events);
        }

        applied
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
    fn a_snapshot_releases_a_key_the_host_still_holds() {
        let mut host = held(&[key(30, true), key(31, true)]);
        let client = held(&[key(31, true)]);

        let applied = converge(&mut host, &client);

        assert_eq!(applied, vec![key(30, false)]);
        assert_eq!(host, client);
    }

    #[test]
    fn a_snapshot_presses_a_key_the_host_never_saw_go_down() {
        let mut host = HeldInput::new();
        let client = held(&[click(0x110, true), key(57, true)]);

        let applied = converge(&mut host, &client);

        assert_eq!(applied, vec![key(57, true), click(0x110, true)]);
        assert_eq!(host, client);
    }

    #[test]
    fn a_snapshot_that_matches_changes_nothing() {
        let client = held(&[
            key(30, true),
            pad(0, GamepadEvent::Connected),
            press(0, GamepadButton::SOUTH),
            axis(0, GamepadAxis::LEFT_TRIGGER, 0.25),
        ]);
        let mut host = client.clone();

        assert_eq!(converge(&mut host, &client), Vec::new());
    }

    #[test]
    fn a_snapshot_connects_disconnects_and_repairs_pads() {
        let mut host = held(&[
            pad(0, GamepadEvent::Connected),
            press(0, GamepadButton::SOUTH),
            axis(0, GamepadAxis::LEFT_X, 1.0),
            pad(2, GamepadEvent::Connected),
        ]);
        let client = held(&[
            pad(0, GamepadEvent::Connected),
            press(0, GamepadButton::EAST),
            axis(0, GamepadAxis::LEFT_X, -0.5),
            pad(3, GamepadEvent::Connected),
            press(3, GamepadButton::NORTH),
        ]);

        let applied = converge(&mut host, &client);

        assert_eq!(host, client);
        assert!(applied.contains(&pad(2, GamepadEvent::Disconnected)));
        assert!(applied.contains(&pad(3, GamepadEvent::Connected)));
        assert!(applied.contains(&axis(0, GamepadAxis::LEFT_X, -0.5)));
        assert!(applied.contains(&pad(
            0,
            GamepadEvent::Button {
                button: GamepadButton::SOUTH,
                pressed: false
            }
        )));
    }

    #[test]
    fn releases_are_ordered_before_presses() {
        let mut host = held(&[key(1, true)]);
        let client = held(&[key(2, true)]);

        let applied = converge(&mut host, &client);

        assert_eq!(applied, vec![key(1, false), key(2, true)]);
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

    fn locks(caps: bool, num: bool) -> InputEvent {
        InputEvent::Locks(LockState { caps, num })
    }

    #[test]
    fn lock_state_is_unknown_until_the_client_reports_it() {
        let mut records = Vec::new();
        let mut known = HeldInput::new();
        known.observe(&locks(true, false));

        HeldInput::new().snapshot(&mut records);
        assert!(!records.iter().any(|r| matches!(r, StateRecord::Locks(_))));

        known.snapshot(&mut records);
        assert!(records.contains(&StateRecord::Locks(LockState {
            caps: true,
            num: false
        })));
    }

    #[test]
    fn a_snapshot_repairs_lock_state_and_leaves_agreement_alone() {
        let client = held(&[locks(false, true)]);
        let mut never_told = HeldInput::new();
        let mut stale = held(&[locks(true, true)]);
        let mut agreeing = client.clone();

        assert_eq!(converge(&mut never_told, &client), vec![locks(false, true)]);
        assert_eq!(converge(&mut stale, &client), vec![locks(false, true)]);
        assert_eq!(converge(&mut agreeing, &client), Vec::new());
    }

    #[test]
    fn releasing_everything_leaves_the_locks_alone() {
        let state = held(&[locks(true, true)]);
        let mut events = Vec::new();

        state.release_all(&mut events);

        assert_eq!(events, Vec::new());
        assert!(state.is_idle());
    }

    fn touch(slot: u8, event: TouchEvent) -> InputEvent {
        InputEvent::Touch { slot, event }
    }

    fn lift(slot: u8) -> InputEvent {
        touch(slot, TouchEvent::Up)
    }

    fn land(slot: u8) -> InputEvent {
        touch(slot, TouchEvent::Down { x: 0.5, y: 0.5 })
    }

    #[test]
    fn touches_are_held_from_down_until_up_or_cancel() {
        let mut state = held(&[land(0), land(3), land(5)]);
        state.observe(&touch(3, TouchEvent::Motion { x: 0.1, y: 0.2 }));
        state.observe(&lift(0));
        state.observe(&touch(5, TouchEvent::Cancel));

        assert_eq!(state.touches().iter().collect::<Vec<_>>(), vec![3]);
        assert!(!state.is_idle());
    }

    #[test]
    fn a_snapshot_lifts_touches_the_client_no_longer_holds() {
        let mut host = held(&[land(1), land(2)]);
        let client = held(&[land(2)]);

        assert_eq!(converge(&mut host, &client), vec![lift(1)]);
        assert_eq!(host, client);
    }

    #[test]
    fn a_snapshot_never_invents_a_touch() {
        let mut host = HeldInput::new();
        let client = held(&[land(2)]);

        assert_eq!(converge(&mut host, &client), Vec::new());
    }

    #[test]
    fn releasing_everything_lifts_touches() {
        let state = held(&[land(4), land(7), key(30, true)]);
        let mut events = Vec::new();

        state.release_all(&mut events);

        assert_eq!(events, vec![key(30, false), lift(4), lift(7)]);
    }

    #[test]
    fn an_idle_snapshot_still_names_every_scope() {
        let mut records = Vec::new();

        HeldInput::new().snapshot(&mut records);

        assert_eq!(
            records,
            vec![
                StateRecord::Keys(KeySet::default()),
                StateRecord::Buttons(KeySet::default()),
                StateRecord::Pads(0),
                StateRecord::Touches(TouchSet::default()),
            ]
        );
    }
}
