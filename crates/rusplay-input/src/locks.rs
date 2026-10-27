use std::time::{Duration, Instant};
#[cfg(any(feature = "wayland", feature = "libei"))]
use std::{
    fs::File,
    os::{fd::OwnedFd, unix::fs::FileExt},
};

use crate::InputError;

pub const KEY_CAPS_LOCK: u32 = 58;
pub const KEY_NUM_LOCK: u32 = 69;

const CAPS_KEYSYM: &str = "Caps_Lock";
const NUM_KEYSYM: &str = "Num_Lock";

const STANDARD_CAPS_MASK: u32 = 1 << 1;
const STANDARD_NUM_MASK: u32 = 1 << 4;

const CAPS_BIT: u8 = 1;
const NUM_BIT: u8 = 2;

const REAL_MODIFIERS: [(&str, u32); 8] = [
    ("Shift", 1 << 0),
    ("Lock", 1 << 1),
    ("Control", 1 << 2),
    ("Mod1", 1 << 3),
    ("Mod2", 1 << 4),
    ("Mod3", 1 << 5),
    ("Mod4", 1 << 6),
    ("Mod5", 1 << 7),
];

#[cfg(any(feature = "wayland", feature = "libei"))]
const MAX_KEYMAP_LEN: u32 = 4 * 1024 * 1024;

pub const LOCK_SETTLE: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockState {
    pub caps: bool,
    pub num: bool,
}

impl LockState {
    pub const fn to_bits(self) -> u8 {
        (if self.caps { CAPS_BIT } else { 0 }) | (if self.num { NUM_BIT } else { 0 })
    }

    pub fn from_bits(bits: u8) -> Result<Self, InputError> {
        if bits & !(CAPS_BIT | NUM_BIT) != 0 {
            return Err(InputError::WireInvalidLockBits(bits));
        }

        Ok(Self {
            caps: bits & CAPS_BIT != 0,
            num: bits & NUM_BIT != 0,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockMasks {
    pub caps: u32,
    pub num: u32,
}

impl Default for LockMasks {
    fn default() -> Self {
        Self {
            caps: STANDARD_CAPS_MASK,
            num: STANDARD_NUM_MASK,
        }
    }
}

impl LockMasks {
    pub fn from_keymap(text: &str) -> Self {
        let standard = Self::default();

        Self {
            caps: modifier_mask_of(text, CAPS_KEYSYM).unwrap_or(standard.caps),
            num: modifier_mask_of(text, NUM_KEYSYM).unwrap_or(standard.num),
        }
    }

    pub const fn locked(self, locked_mods: u32) -> LockState {
        LockState {
            caps: locked_mods & self.caps != 0,
            num: locked_mods & self.num != 0,
        }
    }
}

fn modifier_mask_of(text: &str, keysym: &str) -> Option<u32> {
    let keys = keys_producing(text, keysym);
    let mask = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("modifier_map "))
        .filter_map(|rest| rest.split_once('{'))
        .filter(|(_, members)| keys.iter().any(|key| members.contains(&format!("<{key}>"))))
        .filter_map(|(name, _)| {
            REAL_MODIFIERS
                .iter()
                .find(|(real, _)| *real == name.trim())
                .map(|(_, bit)| *bit)
        })
        .fold(0, |mask, bit| mask | bit);

    (mask != 0).then_some(mask)
}

fn keys_producing<'a>(text: &'a str, keysym: &str) -> Vec<&'a str> {
    let mut names = Vec::new();
    let mut open: Option<&str> = None;
    let mut matched = false;

    for line in text.lines().map(str::trim) {
        let chunk = if open.is_some() {
            line
        } else if let Some((name, tail)) = line
            .strip_prefix("key <")
            .and_then(|rest| rest.split_once('>'))
        {
            open = Some(name);
            matched = false;
            tail
        } else {
            continue;
        };

        matched |= has_token(chunk, keysym);
        if chunk.contains("};")
            && let Some(name) = open.take()
            && matched
        {
            names.push(name);
        }
    }

    names
}

fn has_token(text: &str, token: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|candidate| candidate == token)
}

#[cfg(any(feature = "wayland", feature = "libei"))]
pub(crate) fn read_keymap(fd: &OwnedFd, size: u32) -> Option<String> {
    if size == 0 || size > MAX_KEYMAP_LEN {
        return None;
    }

    let file = File::from(fd.try_clone().ok()?);
    let mut bytes = vec![0; usize::try_from(size).ok()?];
    file.read_exact_at(&mut bytes, 0).ok()?;
    while bytes.last() == Some(&0) {
        bytes.pop();
    }

    String::from_utf8(bytes).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockTaps {
    pub caps: bool,
    pub num: bool,
}

impl LockTaps {
    pub const fn is_empty(self) -> bool {
        !self.caps && !self.num
    }

    pub fn keycodes(self) -> impl Iterator<Item = u32> {
        [(self.caps, KEY_CAPS_LOCK), (self.num, KEY_NUM_LOCK)]
            .into_iter()
            .filter_map(|(tap, keycode)| tap.then_some(keycode))
    }
}

#[derive(Debug, Clone, Default)]
pub struct LockSync {
    desired: Option<LockState>,
    reported: Option<LockState>,
    quiet_until: Option<Instant>,
    awaiting_report: bool,
}

impl LockSync {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn desire(&mut self, locks: LockState) {
        self.desired = Some(locks);
    }

    pub fn report(&mut self, locks: LockState) {
        self.reported = Some(locks);
        self.awaiting_report = false;
    }

    pub fn key_injected(&mut self, keycode: u32, now: Instant) {
        if keycode == KEY_CAPS_LOCK || keycode == KEY_NUM_LOCK {
            self.quiet_until = Some(now + LOCK_SETTLE);
        }
    }

    pub fn due(&mut self, now: Instant) -> LockTaps {
        let (Some(desired), Some(reported)) = (self.desired, self.reported) else {
            return LockTaps::default();
        };

        if desired == reported {
            self.desired = None;
            return LockTaps::default();
        }

        if self.awaiting_report || self.quiet_until.is_some_and(|until| now < until) {
            return LockTaps::default();
        }

        LockTaps {
            caps: desired.caps != reported.caps,
            num: desired.num != reported.num,
        }
    }

    pub fn tapped(&mut self, now: Instant) {
        self.desired = None;
        self.awaiting_report = true;
        self.quiet_until = Some(now + LOCK_SETTLE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US_KEYMAP: &str = "xkb_keymap {
xkb_symbols \"pc+us\" {
	key <CAPS>               {	[       Caps_Lock ] };
	key <FK01>               {
		type= \"CTRL+ALT\",
		symbols[1]= [              F1,              F1 ]
	};
	key <NMLK>               {	[        Num_Lock ] };
	modifier_map Shift { <LFSH>, <RTSH> };
	modifier_map Lock { <CAPS> };
	modifier_map Control { <LCTL>, <RCTL> };
	modifier_map Mod1 { <LALT>, <RALT>, <ALT>, <META> };
	modifier_map Mod2 { <NMLK> };
	modifier_map Mod4 { <LWIN>, <RWIN>, <SUPR> };
};
};";

    const MOVED_KEYMAP: &str = "xkb_symbols \"odd\" {
	key <CAPS>               {
		symbols[1]= [ Caps_Lock ]
	};
	key <NMLK>               {	[        Num_Lock ] };
	key <KP0>                {	[        KP_0 ] };
	modifier_map Mod3 { <NMLK> };
	modifier_map Mod2 { <KP0> };
};";

    fn lock(caps: bool, num: bool) -> LockState {
        LockState { caps, num }
    }

    #[test]
    fn standard_masks_are_lock_and_mod2() {
        let masks = LockMasks::default();

        assert_eq!(masks.caps, 0b10);
        assert_eq!(masks.num, 0b1_0000);
    }

    #[test]
    fn the_masks_follow_the_keymap_modifier_map() {
        let us = LockMasks::from_keymap(US_KEYMAP);
        let moved = LockMasks::from_keymap(MOVED_KEYMAP);

        assert_eq!(us, LockMasks::default());
        assert_eq!(moved.num, 1 << 5);
        assert_eq!(moved.caps, STANDARD_CAPS_MASK);
    }

    #[test]
    fn a_keymap_without_the_lock_keys_falls_back_to_the_standard_masks() {
        assert_eq!(LockMasks::from_keymap(""), LockMasks::default());
        assert_eq!(
            LockMasks::from_keymap("key <A> { [ a ] };"),
            LockMasks::default()
        );
    }

    #[test]
    fn locked_masks_map_to_the_lock_state() {
        let masks = LockMasks::default();

        assert_eq!(masks.locked(0), lock(false, false));
        assert_eq!(masks.locked(0b10), lock(true, false));
        assert_eq!(masks.locked(0b1_0000), lock(false, true));
        assert_eq!(masks.locked(0b1_0010 | 1), lock(true, true));
        assert_eq!(masks.locked(1 << 2), lock(false, false));
    }

    #[test]
    fn lock_bits_round_trip_and_reject_unknown_bits() {
        for caps in [false, true] {
            for num in [false, true] {
                let state = lock(caps, num);

                assert_eq!(LockState::from_bits(state.to_bits()).unwrap(), state);
            }
        }

        assert!(matches!(
            LockState::from_bits(4),
            Err(InputError::WireInvalidLockBits(4))
        ));
    }

    #[test]
    fn tapping_waits_for_a_report_and_for_the_settle_time() {
        let start = Instant::now();
        let mut sync = LockSync::new();

        sync.desire(lock(true, false));
        assert!(sync.due(start).is_empty());

        sync.report(lock(false, false));
        sync.key_injected(KEY_CAPS_LOCK, start);
        assert!(sync.due(start + LOCK_SETTLE / 2).is_empty());

        let taps = sync.due(start + LOCK_SETTLE);
        assert_eq!(
            taps,
            LockTaps {
                caps: true,
                num: false
            }
        );
        assert_eq!(taps.keycodes().collect::<Vec<_>>(), vec![KEY_CAPS_LOCK]);
    }

    #[test]
    fn a_tap_is_not_repeated_before_the_server_reports() {
        let start = Instant::now();
        let mut sync = LockSync::new();
        sync.report(lock(false, true));
        sync.desire(lock(false, false));

        let taps = sync.due(start);
        sync.tapped(start);

        assert_eq!(taps.keycodes().collect::<Vec<_>>(), vec![KEY_NUM_LOCK]);
        sync.desire(lock(false, false));
        assert!(sync.due(start + LOCK_SETTLE * 4).is_empty());

        sync.report(lock(false, false));
        assert!(sync.due(start + LOCK_SETTLE * 4).is_empty());
    }

    #[test]
    fn an_agreeing_report_consumes_the_desire() {
        let start = Instant::now();
        let mut sync = LockSync::new();
        sync.report(lock(true, true));
        sync.desire(lock(true, true));

        assert!(sync.due(start).is_empty());

        sync.report(lock(false, true));
        assert!(sync.due(start + LOCK_SETTLE * 2).is_empty());
    }

    #[test]
    fn other_keys_do_not_open_a_settle_window() {
        let start = Instant::now();
        let mut sync = LockSync::new();
        sync.report(lock(false, false));
        sync.desire(lock(false, true));

        sync.key_injected(30, start);

        assert!(!sync.due(start).is_empty());
    }

    #[cfg(any(feature = "wayland", feature = "libei"))]
    #[test]
    fn a_keymap_is_read_from_a_descriptor_without_moving_its_offset() {
        use std::io::Write;

        let mut file = tempfile::tempfile().unwrap();
        file.write_all(US_KEYMAP.as_bytes()).unwrap();
        file.write_all(&[0]).unwrap();
        let fd = OwnedFd::from(file);
        let size = u32::try_from(US_KEYMAP.len() + 1).unwrap();

        assert_eq!(read_keymap(&fd, size).as_deref(), Some(US_KEYMAP));
        assert_eq!(read_keymap(&fd, size).as_deref(), Some(US_KEYMAP));
        assert_eq!(read_keymap(&fd, 0), None);
        assert_eq!(read_keymap(&fd, MAX_KEYMAP_LEN + 1), None);
    }
}
