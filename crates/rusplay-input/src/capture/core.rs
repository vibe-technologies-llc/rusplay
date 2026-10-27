use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use rusplay_config::{ChordModifiers, HotkeyKey, InputConfig, PointerMode, ReleaseChord};

use crate::InputEvent;

const KEY_LEFT_CTRL: u32 = 29;
const KEY_LEFT_SHIFT: u32 = 42;
const KEY_RIGHT_SHIFT: u32 = 54;
const KEY_LEFT_ALT: u32 = 56;
const KEY_RIGHT_CTRL: u32 = 97;
const KEY_RIGHT_ALT: u32 = 100;

const PENDING_ACTION_LIMIT: usize = 8;

const CTRL: [u32; 2] = [KEY_LEFT_CTRL, KEY_RIGHT_CTRL];
const SHIFT: [u32; 2] = [KEY_LEFT_SHIFT, KEY_RIGHT_SHIFT];
const ALT: [u32; 2] = [KEY_LEFT_ALT, KEY_RIGHT_ALT];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl StreamRegion {
    fn normalise(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        if self.width <= 0.0 || self.height <= 0.0 {
            return None;
        }

        Some((
            ((x - self.x) / self.width).clamp(0.0, 1.0),
            ((y - self.y) / self.height).clamp(0.0, 1.0),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkeys {
    pub chord: ChordModifiers,
    pub fullscreen: u32,
    pub stats: u32,
    pub disconnect: u32,
    pub pointer_mode: u32,
}

impl From<&InputConfig> for Hotkeys {
    fn from(config: &InputConfig) -> Self {
        Self {
            chord: config.hotkey_chord.modifiers(),
            fullscreen: keycode(config.hotkey_fullscreen),
            stats: keycode(config.hotkey_stats),
            disconnect: keycode(config.hotkey_disconnect),
            pointer_mode: keycode(config.hotkey_pointer_mode),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAction {
    ToggleFullscreen,
    ToggleStats,
    Disconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hotkey {
    Client(ClientAction),
    CyclePointerMode,
}

const fn keycode(key: HotkeyKey) -> u32 {
    match key {
        HotkeyKey::A => 30,
        HotkeyKey::B => 48,
        HotkeyKey::C => 46,
        HotkeyKey::D => 32,
        HotkeyKey::E => 18,
        HotkeyKey::F => 33,
        HotkeyKey::G => 34,
        HotkeyKey::H => 35,
        HotkeyKey::I => 23,
        HotkeyKey::J => 36,
        HotkeyKey::K => 37,
        HotkeyKey::L => 38,
        HotkeyKey::M => 50,
        HotkeyKey::N => 49,
        HotkeyKey::O => 24,
        HotkeyKey::P => 25,
        HotkeyKey::Q => 16,
        HotkeyKey::R => 19,
        HotkeyKey::S => 31,
        HotkeyKey::T => 20,
        HotkeyKey::U => 22,
        HotkeyKey::V => 47,
        HotkeyKey::W => 17,
        HotkeyKey::X => 45,
        HotkeyKey::Y => 21,
        HotkeyKey::Z => 44,
        HotkeyKey::F1 => 59,
        HotkeyKey::F2 => 60,
        HotkeyKey::F3 => 61,
        HotkeyKey::F4 => 62,
        HotkeyKey::F5 => 63,
        HotkeyKey::F6 => 64,
        HotkeyKey::F7 => 65,
        HotkeyKey::F8 => 66,
        HotkeyKey::F9 => 67,
        HotkeyKey::F10 => 68,
        HotkeyKey::F11 => 87,
        HotkeyKey::F12 => 88,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureOptions {
    pub keyboard: bool,
    pub pointer: bool,
    pub pointer_mode: PointerMode,
    pub sensitivity: f64,
    pub chord: ReleaseChord,
    pub hotkeys: Option<Hotkeys>,
    pub inhibit_shortcuts: bool,
    pub auto_delay: Duration,
}

impl From<&InputConfig> for CaptureOptions {
    fn from(config: &InputConfig) -> Self {
        Self {
            keyboard: config.keyboard,
            pointer: config.pointer,
            pointer_mode: config.pointer_mode,
            sensitivity: f64::from(config.pointer_sensitivity),
            chord: config.release_chord,
            hotkeys: config.hotkeys.then(|| Hotkeys::from(config)),
            inhibit_shortcuts: config.inhibit_shortcuts,
            auto_delay: Duration::from_millis(u64::from(config.pointer_auto_delay_ms)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Unchanged,
    Engaged,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePointer {
    Absolute,
    Relative,
}

#[derive(Debug, Clone, Copy)]
struct PendingSwitch {
    target: ActivePointer,
    since: Instant,
}

#[derive(Debug, Clone)]
pub struct CaptureCore {
    options: CaptureOptions,
    mode: PointerMode,
    active: ActivePointer,
    relative_available: bool,
    host_cursor_visible: Option<bool>,
    pending: Option<PendingSwitch>,
    engaged: bool,
    region: Option<StreamRegion>,
    down: BTreeSet<u32>,
    held_keys: BTreeSet<u32>,
    held_buttons: BTreeSet<u32>,
    suppressed_keys: BTreeSet<u32>,
    suppressed_buttons: BTreeSet<u32>,
    actions: Vec<ClientAction>,
}

impl CaptureCore {
    pub fn new(options: CaptureOptions) -> Self {
        let active = match options.pointer_mode {
            PointerMode::Relative => ActivePointer::Relative,
            PointerMode::Absolute | PointerMode::Auto => ActivePointer::Absolute,
        };

        Self {
            options,
            mode: options.pointer_mode,
            active,
            relative_available: true,
            host_cursor_visible: None,
            pending: None,
            engaged: false,
            region: None,
            down: BTreeSet::new(),
            held_keys: BTreeSet::new(),
            held_buttons: BTreeSet::new(),
            suppressed_keys: BTreeSet::new(),
            suppressed_buttons: BTreeSet::new(),
            actions: Vec::new(),
        }
    }

    pub fn options(&self) -> CaptureOptions {
        self.options
    }

    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    pub fn mode(&self) -> PointerMode {
        self.mode
    }

    pub fn active_pointer(&self) -> ActivePointer {
        self.active
    }

    pub fn take_actions(&mut self) -> Vec<ClientAction> {
        std::mem::take(&mut self.actions)
    }

    pub fn wants_pointer_lock(&self) -> bool {
        self.engaged && self.active == ActivePointer::Relative
    }

    pub fn set_region(&mut self, region: Option<StreamRegion>) {
        self.region = region;
    }

    pub fn set_relative_available(&mut self, available: bool) {
        self.relative_available = available;
        if !available && self.active == ActivePointer::Relative {
            self.active = ActivePointer::Absolute;
        }
        if !available && self.mode == PointerMode::Relative {
            self.mode = PointerMode::Absolute;
        }
    }

    pub fn set_mode(&mut self, mode: PointerMode) -> bool {
        if mode == PointerMode::Relative && !self.relative_available {
            return false;
        }

        self.mode = mode;
        self.pending = None;
        match mode {
            PointerMode::Absolute => self.active = ActivePointer::Absolute,
            PointerMode::Relative => self.active = ActivePointer::Relative,
            PointerMode::Auto => {
                if let Some(visible) = self.host_cursor_visible {
                    self.active = self.pointer_for_host_cursor(visible);
                }
            }
        }

        true
    }

    pub fn cycle_mode(&mut self) {
        let next = match (self.mode, self.relative_available) {
            (PointerMode::Absolute, true) => PointerMode::Relative,
            (PointerMode::Absolute, false) | (PointerMode::Relative, _) => PointerMode::Auto,
            (PointerMode::Auto, _) => PointerMode::Absolute,
        };

        self.set_mode(next);
    }

    pub fn observe_host_cursor(&mut self, visible: bool, now: Instant) {
        self.host_cursor_visible = Some(visible);
        if self.mode != PointerMode::Auto {
            return;
        }

        let target = self.pointer_for_host_cursor(visible);
        if target == self.active {
            self.pending = None;
        } else if self.pending.is_none_or(|pending| pending.target != target) {
            self.pending = Some(PendingSwitch { target, since: now });
        }
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        let Some(pending) = self.pending else {
            return false;
        };
        if now.saturating_duration_since(pending.since) < self.options.auto_delay {
            return false;
        }

        self.pending = None;
        self.active = pending.target;

        true
    }

    fn pointer_for_host_cursor(&self, visible: bool) -> ActivePointer {
        if visible || !self.relative_available {
            ActivePointer::Absolute
        } else {
            ActivePointer::Relative
        }
    }

    pub fn engage(&mut self) -> Transition {
        if self.engaged {
            return Transition::Unchanged;
        }

        self.engaged = true;
        self.suppressed_keys.clone_from(&self.down);

        Transition::Engaged
    }

    pub fn release(&mut self, out: &mut Vec<InputEvent>) -> Transition {
        if !self.engaged {
            return Transition::Unchanged;
        }

        self.engaged = false;
        out.extend(
            std::mem::take(&mut self.held_keys)
                .into_iter()
                .map(|keycode| InputEvent::Key {
                    keycode,
                    pressed: false,
                }),
        );
        out.extend(
            std::mem::take(&mut self.held_buttons)
                .into_iter()
                .map(|button| InputEvent::PointerButton {
                    button,
                    pressed: false,
                }),
        );
        self.suppressed_keys.clear();
        self.suppressed_buttons.clear();

        Transition::Released
    }

    pub fn key(&mut self, keycode: u32, pressed: bool, out: &mut Vec<InputEvent>) -> Transition {
        if pressed {
            self.down.insert(keycode);
        } else {
            self.down.remove(&keycode);
        }

        if pressed && self.completes_chord(keycode) {
            return if self.engaged {
                self.release(out)
            } else {
                self.engage()
            };
        }

        if !self.engaged {
            return Transition::Unchanged;
        }

        if pressed && let Some(hotkey) = self.hotkey_for(keycode) {
            self.run(hotkey);

            return Transition::Unchanged;
        }

        if !self.options.keyboard {
            return Transition::Unchanged;
        }

        if pressed {
            self.held_keys.insert(keycode);
            out.push(InputEvent::Key {
                keycode,
                pressed: true,
            });
        } else if !self.suppressed_keys.remove(&keycode) && self.held_keys.remove(&keycode) {
            out.push(InputEvent::Key {
                keycode,
                pressed: false,
            });
        }

        Transition::Unchanged
    }

    pub fn pointer_button(
        &mut self,
        button: u32,
        pressed: bool,
        out: &mut Vec<InputEvent>,
    ) -> Transition {
        if !self.engaged {
            if !pressed {
                return Transition::Unchanged;
            }

            self.engage();
            self.suppressed_buttons.insert(button);

            return Transition::Engaged;
        }

        if !self.options.pointer {
            return Transition::Unchanged;
        }

        if pressed {
            self.held_buttons.insert(button);
            out.push(InputEvent::PointerButton {
                button,
                pressed: true,
            });
        } else if !self.suppressed_buttons.remove(&button) && self.held_buttons.remove(&button) {
            out.push(InputEvent::PointerButton {
                button,
                pressed: false,
            });
        }

        Transition::Unchanged
    }

    pub fn pointer_motion(&mut self, dx: f64, dy: f64, out: &mut Vec<InputEvent>) {
        if !self.forwards_pointer(ActivePointer::Relative) || !dx.is_finite() || !dy.is_finite() {
            return;
        }

        out.push(InputEvent::PointerMotion {
            dx: dx * self.options.sensitivity,
            dy: dy * self.options.sensitivity,
        });
    }

    pub fn pointer_position(&mut self, x: f64, y: f64, out: &mut Vec<InputEvent>) {
        if !self.forwards_pointer(ActivePointer::Absolute) || !x.is_finite() || !y.is_finite() {
            return;
        }

        if let Some((x, y)) = self.region.and_then(|region| region.normalise(x, y)) {
            out.push(InputEvent::PointerAbsolute { x, y });
        }
    }

    pub fn scroll(&mut self, dx: f64, dy: f64, out: &mut Vec<InputEvent>) {
        if !self.engaged || !self.options.pointer || !dx.is_finite() || !dy.is_finite() {
            return;
        }

        out.push(InputEvent::Scroll { dx, dy });
    }

    fn forwards_pointer(&self, pointer: ActivePointer) -> bool {
        self.engaged && self.options.pointer && self.active == pointer
    }

    fn hotkey_for(&self, keycode: u32) -> Option<Hotkey> {
        let hotkeys = self.options.hotkeys?;
        if !self.holds(hotkeys.chord) {
            return None;
        }

        match keycode {
            key if key == hotkeys.fullscreen => {
                Some(Hotkey::Client(ClientAction::ToggleFullscreen))
            }
            key if key == hotkeys.stats => Some(Hotkey::Client(ClientAction::ToggleStats)),
            key if key == hotkeys.disconnect => Some(Hotkey::Client(ClientAction::Disconnect)),
            key if key == hotkeys.pointer_mode => Some(Hotkey::CyclePointerMode),
            _ => None,
        }
    }

    fn run(&mut self, hotkey: Hotkey) {
        match hotkey {
            Hotkey::Client(action) => {
                if self.actions.len() < PENDING_ACTION_LIMIT {
                    self.actions.push(action);
                }
            }
            Hotkey::CyclePointerMode => self.cycle_mode(),
        }
    }

    fn holds(&self, modifiers: ChordModifiers) -> bool {
        let groups = [
            (modifiers.ctrl, CTRL),
            (modifiers.shift, SHIFT),
            (modifiers.alt, ALT),
        ];

        groups
            .iter()
            .filter(|(required, _)| *required)
            .all(|(_, keys)| keys.iter().any(|key| self.down.contains(key)))
    }

    fn completes_chord(&self, keycode: u32) -> bool {
        let modifiers = self.options.chord.modifiers();
        let completing = [
            (modifiers.ctrl, CTRL),
            (modifiers.shift, SHIFT),
            (modifiers.alt, ALT),
        ]
        .iter()
        .any(|(required, keys)| *required && keys.contains(&keycode));

        completing && self.holds(modifiers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: u32 = 30;
    const KEY_S: u32 = 31;
    const KEY_D: u32 = 32;
    const KEY_F: u32 = 33;
    const KEY_P: u32 = 25;
    const BTN_LEFT: u32 = 0x110;

    fn options() -> CaptureOptions {
        CaptureOptions {
            keyboard: true,
            pointer: true,
            pointer_mode: PointerMode::Relative,
            sensitivity: 2.0,
            chord: ReleaseChord::CtrlAlt,
            hotkeys: None,
            inhibit_shortcuts: true,
            auto_delay: Duration::from_millis(100),
        }
    }

    fn engaged(options: CaptureOptions) -> CaptureCore {
        let mut core = CaptureCore::new(options);
        core.engage();
        core
    }

    fn key(keycode: u32, pressed: bool) -> InputEvent {
        InputEvent::Key { keycode, pressed }
    }

    #[test]
    fn nothing_is_forwarded_until_engaged() {
        let mut core = CaptureCore::new(options());
        let mut out = Vec::new();

        core.key(KEY_A, true, &mut out);
        core.pointer_motion(1.0, 1.0, &mut out);
        core.scroll(0.0, 1.0, &mut out);

        assert_eq!(out, Vec::new());
    }

    #[test]
    fn clicking_engages_and_swallows_the_click() {
        let mut core = CaptureCore::new(options());
        let mut out = Vec::new();

        let press = core.pointer_button(BTN_LEFT, true, &mut out);
        let release = core.pointer_button(BTN_LEFT, false, &mut out);
        core.pointer_motion(1.0, -1.0, &mut out);

        assert_eq!(press, Transition::Engaged);
        assert_eq!(release, Transition::Unchanged);
        assert!(core.is_engaged());
        assert_eq!(out, vec![InputEvent::PointerMotion { dx: 2.0, dy: -2.0 }]);
    }

    #[test]
    fn keys_and_buttons_are_forwarded_while_engaged() {
        let mut core = engaged(options());
        let mut out = Vec::new();

        core.key(KEY_A, true, &mut out);
        core.key(KEY_A, false, &mut out);
        core.pointer_button(BTN_LEFT, true, &mut out);
        core.pointer_button(BTN_LEFT, false, &mut out);
        core.scroll(0.0, 1.5, &mut out);

        assert_eq!(
            out,
            vec![
                key(KEY_A, true),
                key(KEY_A, false),
                InputEvent::PointerButton {
                    button: BTN_LEFT,
                    pressed: true
                },
                InputEvent::PointerButton {
                    button: BTN_LEFT,
                    pressed: false
                },
                InputEvent::Scroll { dx: 0.0, dy: 1.5 },
            ]
        );
    }

    #[test]
    fn the_chord_releases_and_lets_go_of_everything_held() {
        let mut core = engaged(options());
        let mut out = Vec::new();
        core.key(KEY_A, true, &mut out);
        core.key(KEY_LEFT_CTRL, true, &mut out);
        core.pointer_button(BTN_LEFT, true, &mut out);
        out.clear();

        let transition = core.key(KEY_LEFT_ALT, true, &mut out);

        assert_eq!(transition, Transition::Released);
        assert!(!core.is_engaged());
        assert_eq!(
            out,
            vec![
                key(KEY_LEFT_CTRL, false),
                key(KEY_A, false),
                InputEvent::PointerButton {
                    button: BTN_LEFT,
                    pressed: false
                },
            ]
        );
    }

    #[test]
    fn the_chord_completes_with_either_side_in_any_order() {
        let mut core = engaged(options());
        let mut out = Vec::new();

        core.key(KEY_RIGHT_ALT, true, &mut out);
        let transition = core.key(KEY_RIGHT_CTRL, true, &mut out);

        assert_eq!(transition, Transition::Released);
    }

    #[test]
    fn a_partial_chord_does_not_release() {
        let mut core = engaged(CaptureOptions {
            chord: ReleaseChord::CtrlAltShift,
            ..options()
        });
        let mut out = Vec::new();

        core.key(KEY_LEFT_CTRL, true, &mut out);
        let transition = core.key(KEY_LEFT_ALT, true, &mut out);

        assert_eq!(transition, Transition::Unchanged);
        assert!(core.is_engaged());
    }

    #[test]
    fn a_non_modifier_key_never_completes_the_chord() {
        let mut core = engaged(options());
        let mut out = Vec::new();
        core.key(KEY_LEFT_CTRL, true, &mut out);
        core.key(KEY_LEFT_ALT, true, &mut out);
        core.release(&mut out);
        out.clear();

        let transition = core.key(KEY_A, true, &mut out);

        assert_eq!(transition, Transition::Unchanged);
        assert_eq!(out, Vec::new());
    }

    #[test]
    fn the_chord_engages_again_and_swallows_the_keys_still_held() {
        let mut core = engaged(options());
        let mut out = Vec::new();
        core.key(KEY_LEFT_CTRL, true, &mut out);
        core.key(KEY_LEFT_ALT, true, &mut out);
        core.key(KEY_LEFT_CTRL, false, &mut out);
        core.key(KEY_LEFT_ALT, false, &mut out);
        out.clear();

        core.key(KEY_LEFT_CTRL, true, &mut out);
        let transition = core.key(KEY_LEFT_ALT, true, &mut out);
        core.key(KEY_LEFT_ALT, false, &mut out);
        core.key(KEY_LEFT_CTRL, false, &mut out);
        core.key(KEY_A, true, &mut out);

        assert_eq!(transition, Transition::Engaged);
        assert_eq!(out, vec![key(KEY_A, true)]);
    }

    #[test]
    fn absolute_mode_maps_the_pointer_into_the_stream_region() {
        let mut core = engaged(CaptureOptions {
            pointer_mode: PointerMode::Absolute,
            ..options()
        });
        core.set_region(Some(StreamRegion {
            x: 100.0,
            y: 50.0,
            width: 200.0,
            height: 100.0,
        }));
        let mut out = Vec::new();

        core.pointer_position(200.0, 100.0, &mut out);
        core.pointer_position(0.0, 400.0, &mut out);
        core.pointer_motion(5.0, 5.0, &mut out);

        assert_eq!(
            out,
            vec![
                InputEvent::PointerAbsolute { x: 0.5, y: 0.5 },
                InputEvent::PointerAbsolute { x: 0.0, y: 1.0 },
            ]
        );
    }

    #[test]
    fn absolute_motion_needs_a_usable_region() {
        let mut core = engaged(CaptureOptions {
            pointer_mode: PointerMode::Absolute,
            ..options()
        });
        let mut out = Vec::new();

        core.pointer_position(1.0, 1.0, &mut out);
        core.set_region(Some(StreamRegion {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 10.0,
        }));
        core.pointer_position(1.0, 1.0, &mut out);

        assert_eq!(out, Vec::new());
    }

    #[test]
    fn disabled_devices_are_not_forwarded() {
        let mut core = engaged(CaptureOptions {
            keyboard: false,
            pointer: false,
            ..options()
        });
        let mut out = Vec::new();

        core.key(KEY_A, true, &mut out);
        core.pointer_button(BTN_LEFT, true, &mut out);
        core.pointer_motion(1.0, 1.0, &mut out);
        core.scroll(1.0, 1.0, &mut out);

        assert_eq!(out, Vec::new());
    }

    #[test]
    fn non_finite_motion_is_dropped() {
        let mut core = engaged(options());
        let mut out = Vec::new();

        core.pointer_motion(f64::NAN, 1.0, &mut out);
        core.scroll(0.0, f64::INFINITY, &mut out);

        assert_eq!(out, Vec::new());
    }

    #[test]
    fn options_follow_the_config() {
        let config = InputConfig {
            pointer_sensitivity: 1.5,
            pointer_mode: PointerMode::Absolute,
            ..InputConfig::default()
        };

        let options = CaptureOptions::from(&config);

        assert_eq!(
            options,
            CaptureOptions {
                keyboard: true,
                pointer: true,
                pointer_mode: PointerMode::Absolute,
                sensitivity: 1.5,
                chord: ReleaseChord::CtrlAltShift,
                hotkeys: Some(Hotkeys::from(&config)),
                inhibit_shortcuts: true,
                auto_delay: Duration::from_millis(250),
            }
        );
    }

    #[test]
    fn hotkeys_are_absent_when_the_config_turns_them_off() {
        let config = InputConfig {
            hotkeys: false,
            ..InputConfig::default()
        };

        assert_eq!(CaptureOptions::from(&config).hotkeys, None);
    }

    #[test]
    fn the_default_hotkeys_map_to_evdev_codes() {
        let hotkeys = Hotkeys::from(&InputConfig::default());

        assert_eq!(hotkeys.fullscreen, KEY_F);
        assert_eq!(hotkeys.stats, KEY_S);
        assert_eq!(hotkeys.disconnect, KEY_D);
        assert_eq!(hotkeys.pointer_mode, KEY_P);
        assert_eq!(keycode(HotkeyKey::F11), 87);
        assert_eq!(keycode(HotkeyKey::A), KEY_A);
    }

    fn hotkey_options() -> CaptureOptions {
        CaptureOptions {
            chord: ReleaseChord::CtrlShift,
            hotkeys: Some(Hotkeys::from(&InputConfig::default())),
            ..options()
        }
    }

    fn hotkey_core() -> CaptureCore {
        engaged(hotkey_options())
    }

    #[test]
    fn a_hotkey_queues_its_action_and_never_reaches_the_host() {
        let mut core = hotkey_core();
        let mut out = Vec::new();
        core.key(KEY_LEFT_CTRL, true, &mut out);
        core.key(KEY_LEFT_ALT, true, &mut out);
        out.clear();

        core.key(KEY_F, true, &mut out);
        core.key(KEY_F, false, &mut out);
        core.key(KEY_S, true, &mut out);
        core.key(KEY_D, true, &mut out);

        assert_eq!(out, Vec::new());
        assert_eq!(
            core.take_actions(),
            vec![
                ClientAction::ToggleFullscreen,
                ClientAction::ToggleStats,
                ClientAction::Disconnect,
            ]
        );
        assert_eq!(core.take_actions(), Vec::new());
        assert!(core.is_engaged());
    }

    #[test]
    fn the_pointer_hotkey_cycles_the_mode_inside_the_core() {
        let mut core = hotkey_core();
        let mut out = Vec::new();
        core.key(KEY_RIGHT_CTRL, true, &mut out);
        core.key(KEY_RIGHT_ALT, true, &mut out);

        core.key(KEY_P, true, &mut out);

        assert_eq!(core.mode(), PointerMode::Auto);
        assert_eq!(core.take_actions(), Vec::new());
    }

    #[test]
    fn without_the_whole_chord_the_key_is_forwarded() {
        let mut core = hotkey_core();
        let mut out = Vec::new();

        core.key(KEY_F, true, &mut out);
        core.key(KEY_F, false, &mut out);
        core.key(KEY_LEFT_CTRL, true, &mut out);
        out.clear();
        core.key(KEY_S, true, &mut out);

        assert_eq!(out, vec![key(KEY_S, true)]);
        assert_eq!(core.take_actions(), Vec::new());
    }

    #[test]
    fn hotkeys_do_nothing_while_released_or_when_turned_off() {
        let mut released = CaptureCore::new(hotkey_options());
        let mut off = engaged(CaptureOptions {
            hotkeys: None,
            ..hotkey_options()
        });
        let mut out = Vec::new();

        for core in [&mut released, &mut off] {
            core.key(KEY_LEFT_CTRL, true, &mut out);
            core.key(KEY_LEFT_ALT, true, &mut out);
            core.key(KEY_F, true, &mut out);
        }

        assert_eq!(released.take_actions(), Vec::new());
        assert_eq!(off.take_actions(), Vec::new());
        assert!(out.contains(&key(KEY_F, true)));
    }

    #[test]
    fn hotkeys_work_while_keyboard_forwarding_is_off() {
        let mut core = engaged(CaptureOptions {
            keyboard: false,
            ..hotkey_options()
        });
        let mut out = Vec::new();

        core.key(KEY_LEFT_CTRL, true, &mut out);
        core.key(KEY_LEFT_ALT, true, &mut out);
        core.key(KEY_S, true, &mut out);

        assert_eq!(core.take_actions(), vec![ClientAction::ToggleStats]);
        assert_eq!(out, Vec::new());
    }

    #[test]
    fn unanswered_actions_stop_queueing_at_the_limit() {
        let mut core = hotkey_core();
        let mut out = Vec::new();
        core.key(KEY_LEFT_CTRL, true, &mut out);
        core.key(KEY_LEFT_ALT, true, &mut out);

        for _ in 0..PENDING_ACTION_LIMIT * 2 {
            core.key(KEY_S, true, &mut out);
            core.key(KEY_S, false, &mut out);
        }

        assert_eq!(core.take_actions().len(), PENDING_ACTION_LIMIT);
    }

    fn auto_core() -> CaptureCore {
        engaged(CaptureOptions {
            pointer_mode: PointerMode::Auto,
            ..options()
        })
    }

    fn forwarded_after_motion_and_position(core: &mut CaptureCore) -> Vec<InputEvent> {
        let mut out = Vec::new();
        core.pointer_motion(1.0, 1.0, &mut out);
        core.pointer_position(10.0, 10.0, &mut out);

        out
    }

    #[test]
    fn auto_starts_absolute_and_waits_out_the_delay_before_switching() {
        let mut core = auto_core();
        core.set_region(Some(StreamRegion {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        }));
        let start = Instant::now();

        core.observe_host_cursor(false, start);
        let early = core.tick(start + Duration::from_millis(99));
        let early_pointer = core.active_pointer();
        let switched = core.tick(start + Duration::from_millis(100));

        assert_eq!(core.mode(), PointerMode::Auto);
        assert!(!early);
        assert_eq!(early_pointer, ActivePointer::Absolute);
        assert!(switched);
        assert_eq!(core.active_pointer(), ActivePointer::Relative);
        assert_eq!(
            forwarded_after_motion_and_position(&mut core),
            vec![InputEvent::PointerMotion { dx: 2.0, dy: 2.0 }]
        );
    }

    #[test]
    fn a_flicker_shorter_than_the_delay_never_switches() {
        let mut core = auto_core();
        let start = Instant::now();

        core.observe_host_cursor(false, start);
        core.observe_host_cursor(true, start + Duration::from_millis(40));
        let switched = core.tick(start + Duration::from_millis(500));

        assert!(!switched);
        assert_eq!(core.active_pointer(), ActivePointer::Absolute);
    }

    #[test]
    fn repeated_observations_keep_the_first_deadline() {
        let mut core = auto_core();
        let start = Instant::now();

        core.observe_host_cursor(false, start);
        core.observe_host_cursor(false, start + Duration::from_millis(80));
        let switched = core.tick(start + Duration::from_millis(100));

        assert!(switched);
        assert_eq!(core.active_pointer(), ActivePointer::Relative);
    }

    #[test]
    fn a_manual_mode_overrides_auto_and_ignores_the_host_cursor() {
        let mut core = auto_core();
        let start = Instant::now();

        core.set_mode(PointerMode::Absolute);
        core.observe_host_cursor(false, start);
        let switched = core.tick(start + Duration::from_secs(5));

        assert!(!switched);
        assert_eq!(core.mode(), PointerMode::Absolute);
        assert_eq!(core.active_pointer(), ActivePointer::Absolute);
    }

    #[test]
    fn returning_to_auto_adopts_the_last_host_cursor_at_once() {
        let mut core = engaged(options());
        core.observe_host_cursor(true, Instant::now());

        core.set_mode(PointerMode::Auto);

        assert_eq!(core.active_pointer(), ActivePointer::Absolute);
    }

    #[test]
    fn cycling_walks_absolute_relative_auto() {
        let mut core = engaged(CaptureOptions {
            pointer_mode: PointerMode::Absolute,
            ..options()
        });
        let mut seen = Vec::new();

        for _ in 0..4 {
            core.cycle_mode();
            seen.push((core.mode(), core.active_pointer()));
        }

        assert_eq!(
            seen,
            vec![
                (PointerMode::Relative, ActivePointer::Relative),
                (PointerMode::Auto, ActivePointer::Relative),
                (PointerMode::Absolute, ActivePointer::Absolute),
                (PointerMode::Relative, ActivePointer::Relative),
            ]
        );
    }

    #[test]
    fn cycling_skips_relative_when_it_is_unavailable() {
        let mut core = engaged(CaptureOptions {
            pointer_mode: PointerMode::Absolute,
            ..options()
        });
        core.set_relative_available(false);
        let mut seen = Vec::new();

        for _ in 0..3 {
            core.cycle_mode();
            seen.push(core.mode());
        }

        assert_eq!(
            seen,
            vec![PointerMode::Auto, PointerMode::Absolute, PointerMode::Auto]
        );
    }

    #[test]
    fn without_relative_globals_relative_is_refused_and_auto_stays_absolute() {
        let mut core = auto_core();
        core.set_relative_available(false);
        let start = Instant::now();

        let accepted = core.set_mode(PointerMode::Relative);
        core.observe_host_cursor(false, start);
        let switched = core.tick(start + Duration::from_secs(5));

        assert!(!accepted);
        assert!(!switched);
        assert_eq!(core.active_pointer(), ActivePointer::Absolute);
    }

    #[test]
    fn the_pointer_lock_is_wanted_only_while_engaged_and_relative() {
        let mut core = CaptureCore::new(options());
        let mut out = Vec::new();
        let idle = core.wants_pointer_lock();

        core.engage();
        let relative = core.wants_pointer_lock();
        core.set_mode(PointerMode::Absolute);
        let absolute = core.wants_pointer_lock();
        core.set_mode(PointerMode::Relative);
        core.release(&mut out);
        let released = core.wants_pointer_lock();

        assert!(!idle);
        assert!(relative);
        assert!(!absolute);
        assert!(!released);
    }
}
