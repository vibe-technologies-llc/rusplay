use std::collections::BTreeSet;

use rusplay_config::{InputConfig, PointerMode, ReleaseChord};

use crate::InputEvent;

const KEY_LEFT_CTRL: u32 = 29;
const KEY_LEFT_SHIFT: u32 = 42;
const KEY_RIGHT_SHIFT: u32 = 54;
const KEY_LEFT_ALT: u32 = 56;
const KEY_RIGHT_CTRL: u32 = 97;
const KEY_RIGHT_ALT: u32 = 100;

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureOptions {
    pub keyboard: bool,
    pub pointer: bool,
    pub pointer_mode: PointerMode,
    pub sensitivity: f64,
    pub chord: ReleaseChord,
}

impl From<&InputConfig> for CaptureOptions {
    fn from(config: &InputConfig) -> Self {
        Self {
            keyboard: config.keyboard,
            pointer: config.pointer,
            pointer_mode: config.pointer_mode,
            sensitivity: f64::from(config.pointer_sensitivity),
            chord: config.release_chord,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Unchanged,
    Engaged,
    Released,
}

#[derive(Debug, Clone)]
pub struct CaptureCore {
    options: CaptureOptions,
    engaged: bool,
    region: Option<StreamRegion>,
    down: BTreeSet<u32>,
    held_keys: BTreeSet<u32>,
    held_buttons: BTreeSet<u32>,
    suppressed_keys: BTreeSet<u32>,
    suppressed_buttons: BTreeSet<u32>,
}

impl CaptureCore {
    pub fn new(options: CaptureOptions) -> Self {
        Self {
            options,
            engaged: false,
            region: None,
            down: BTreeSet::new(),
            held_keys: BTreeSet::new(),
            held_buttons: BTreeSet::new(),
            suppressed_keys: BTreeSet::new(),
            suppressed_buttons: BTreeSet::new(),
        }
    }

    pub fn options(&self) -> CaptureOptions {
        self.options
    }

    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    pub fn set_region(&mut self, region: Option<StreamRegion>) {
        self.region = region;
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

        if !self.engaged || !self.options.keyboard {
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
        if !self.forwards_pointer(PointerMode::Relative) || !dx.is_finite() || !dy.is_finite() {
            return;
        }

        out.push(InputEvent::PointerMotion {
            dx: dx * self.options.sensitivity,
            dy: dy * self.options.sensitivity,
        });
    }

    pub fn pointer_position(&mut self, x: f64, y: f64, out: &mut Vec<InputEvent>) {
        if !self.forwards_pointer(PointerMode::Absolute) || !x.is_finite() || !y.is_finite() {
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

    fn forwards_pointer(&self, mode: PointerMode) -> bool {
        self.engaged && self.options.pointer && self.options.pointer_mode == mode
    }

    fn completes_chord(&self, keycode: u32) -> bool {
        let (ctrl, shift, alt) = match self.options.chord {
            ReleaseChord::CtrlAlt => (true, false, true),
            ReleaseChord::CtrlShift => (true, true, false),
            ReleaseChord::AltShift => (false, true, true),
            ReleaseChord::CtrlAltShift => (true, true, true),
        };
        let groups = [(ctrl, CTRL), (shift, SHIFT), (alt, ALT)];

        groups
            .iter()
            .filter(|(required, _)| *required)
            .all(|(_, keys)| keys.iter().any(|key| self.down.contains(key)))
            && groups
                .iter()
                .any(|(required, keys)| *required && keys.contains(&keycode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: u32 = 30;
    const BTN_LEFT: u32 = 0x110;

    fn options() -> CaptureOptions {
        CaptureOptions {
            keyboard: true,
            pointer: true,
            pointer_mode: PointerMode::Relative,
            sensitivity: 2.0,
            chord: ReleaseChord::CtrlAlt,
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
            }
        );
    }
}
