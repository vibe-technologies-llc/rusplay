use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use evdev::{
    AbsoluteAxisCode, Device, EventSummary, FFEffect, FFEffectCode, FFEffectData, FFEffectKind,
    FFReplay, FFTrigger, KeyCode, PropType, SynchronizationCode,
};
use rusplay_config::InputConfig;
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
};

use crate::{
    GamepadEvent, InputError, InputEvent, PadFeedback, RumbleCommand,
    capture::{
        FaceLayout, MotionCaps, MotionTranslator, NodeCaps, NodeDescription, NodeIdentity,
        NodeRole, PadCaps, PadSlots, PadTranslator, RawAxis, RawEvent, SiblingTranslator,
        TouchpadRange, TouchpadTranslator, non_empty, pair_siblings,
    },
};

const INPUT_DIRECTORY: &str = "/dev/input";
const SYSFS_INPUT_CLASS: &str = "/sys/class/input";
const SYSFS_UINPUT_PARENT: &str = "/sys/devices/virtual/input";
const SYSFS_LEDS_CLASS: &str = "/sys/class/leds";
const LIGHTBAR_SUFFIX: &str = "rgb:indicator";
const PLAYER_LED_SUFFIX: &str = "white:player-";
const PLAYER_LED_COUNT: u8 = 5;
const LED_MAX_BRIGHTNESS: &str = "255";
const EVENT_PREFIX: &str = "event";
const SCAN_INTERVAL: Duration = Duration::from_secs(1);
const VENDOR_SONY: u16 = 0x054c;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalOptions {
    pub max_gamepads: u8,
    pub deadzone: f32,
    pub rumble_max_ms: u16,
}

impl From<&InputConfig> for PhysicalOptions {
    fn from(config: &InputConfig) -> Self {
        Self {
            max_gamepads: config.max_gamepads,
            deadzone: config.gamepad_deadzone,
            rumble_max_ms: config.rumble_max_ms,
        }
    }
}

fn drain(device: &mut Device, mut each: impl FnMut(RawEvent)) -> io::Result<()> {
    loop {
        let events: Vec<_> = match device.fetch_events() {
            Ok(events) => events.collect(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        };

        if events.is_empty() {
            return Ok(());
        }

        for event in events {
            match event.destructure() {
                EventSummary::Key(_, code, value) => each(RawEvent::Key {
                    code: code.0,
                    value,
                }),
                EventSummary::AbsoluteAxis(_, code, value) => each(RawEvent::Abs {
                    code: code.0,
                    value,
                }),
                EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _) => {
                    each(RawEvent::Sync);
                }
                _ => {}
            }
        }
    }
}

struct Sibling {
    path: PathBuf,
    role: NodeRole,
    device: Device,
    translator: SiblingTranslator,
}

impl Sibling {
    fn read(&mut self, slot: u8, out: &mut Vec<InputEvent>) -> io::Result<()> {
        let mut events = Vec::new();
        let result = drain(&mut self.device, |raw| {
            self.translator.translate(raw, &mut events);
        });

        out.extend(
            events
                .into_iter()
                .map(|event| InputEvent::Gamepad { slot, event }),
        );
        result
    }

    fn release(&mut self, slot: u8, out: &mut Vec<InputEvent>) {
        let mut events = Vec::new();
        self.translator.release(&mut events);

        out.extend(
            events
                .into_iter()
                .map(|event| InputEvent::Gamepad { slot, event }),
        );
    }
}

struct Pad {
    path: PathBuf,
    slot: u8,
    device: Device,
    translator: PadTranslator,
    rumble_capable: bool,
    effect: Option<FFEffect>,
    siblings: Vec<Sibling>,
}

impl Pad {
    fn read(&mut self, out: &mut Vec<InputEvent>) -> io::Result<()> {
        drain(&mut self.device, |raw| {
            self.translator.translate(self.slot, raw, out);
        })
    }

    fn has_sibling(&self, role: NodeRole) -> bool {
        self.siblings.iter().any(|sibling| sibling.role == role)
    }

    fn play(&mut self, command: &RumbleCommand, limit_ms: u16) -> io::Result<()> {
        if command.is_stop() {
            return self.effect.as_mut().map_or(Ok(()), FFEffect::stop);
        }

        let length = match command.duration_ms {
            RumbleCommand::UNTIL_REPLACED => limit_ms,
            duration => duration.min(limit_ms),
        };
        let data = FFEffectData {
            direction: 0,
            trigger: FFTrigger::default(),
            replay: FFReplay {
                length,
                delay: command.delay_ms,
            },
            kind: FFEffectKind::Rumble {
                strong_magnitude: command.strong,
                weak_magnitude: command.weak,
            },
        };

        let effect = match &mut self.effect {
            Some(effect) => {
                effect.update(data)?;
                effect
            }
            empty => empty.insert(self.device.upload_ff_effect(data)?),
        };

        effect.play(1)
    }
}

pub struct PhysicalGamepads {
    options: PhysicalOptions,
    root: PathBuf,
    sysfs: Option<PathBuf>,
    leds: Option<PathBuf>,
    pads: Vec<Pad>,
    slots: PadSlots<PathBuf>,
    known: BTreeMap<PathBuf, NodeDescription>,
    rejected: BTreeSet<PathBuf>,
    next_scan: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Source {
    pad: usize,
    sibling: Option<usize>,
}

impl PhysicalGamepads {
    pub fn new(options: PhysicalOptions) -> Self {
        Self {
            leds: Some(SYSFS_LEDS_CLASS.into()),
            ..Self::with_roots(options, INPUT_DIRECTORY, Some(SYSFS_INPUT_CLASS.into()))
        }
    }

    #[must_use]
    pub fn with_leds_root(mut self, leds: impl Into<PathBuf>) -> Self {
        self.leds = Some(leds.into());
        self
    }

    pub fn in_directory(options: PhysicalOptions, root: impl Into<PathBuf>) -> Self {
        Self::with_roots(options, root, None)
    }

    pub fn with_roots(
        options: PhysicalOptions,
        root: impl Into<PathBuf>,
        sysfs: Option<PathBuf>,
    ) -> Self {
        Self {
            options,
            root: root.into(),
            sysfs,
            leds: None,
            pads: Vec::new(),
            slots: PadSlots::new(options.max_gamepads),
            known: BTreeMap::new(),
            rejected: BTreeSet::new(),
            next_scan: Instant::now(),
        }
    }

    pub fn connected(&self) -> usize {
        self.pads.len()
    }

    pub fn siblings(&self, slot: u8) -> usize {
        self.pads
            .iter()
            .find(|pad| pad.slot == slot)
            .map_or(0, |pad| pad.siblings.len())
    }

    pub fn poll(&mut self, timeout: Duration, out: &mut Vec<InputEvent>) -> Result<(), InputError> {
        let now = Instant::now();
        if now >= self.next_scan {
            self.scan(out);
            self.next_scan = now + SCAN_INTERVAL;
        }

        let wait = timeout.min(self.next_scan.saturating_duration_since(now));
        let ready = self.wait(wait)?;

        let mut gone_pads = Vec::new();
        let mut gone_siblings = Vec::new();
        for source in ready {
            let pad = &mut self.pads[source.pad];
            match source.sibling {
                None => {
                    if pad.read(out).is_err() {
                        gone_pads.push(source.pad);
                    }
                }
                Some(index) => {
                    let slot = pad.slot;
                    if pad.siblings[index].read(slot, out).is_err() {
                        gone_siblings.push((source.pad, index));
                    }
                }
            }
        }

        for (pad, index) in gone_siblings.into_iter().rev() {
            if !gone_pads.contains(&pad) {
                self.remove_sibling(pad, index, out);
            }
        }
        for index in gone_pads.into_iter().rev() {
            self.remove(index, out);
        }

        Ok(())
    }

    pub fn rumble(&mut self, command: &RumbleCommand) -> Result<(), InputError> {
        if self.options.rumble_max_ms == 0 {
            return Ok(());
        }

        let limit = self.options.rumble_max_ms;
        let Some(pad) = self
            .pads
            .iter_mut()
            .find(|pad| pad.slot == command.slot && pad.rumble_capable)
        else {
            return Ok(());
        };

        pad.play(command, limit)
            .map_err(|source| InputError::PhysicalGamepadRumble {
                slot: command.slot,
                source,
            })
    }

    pub fn apply(&mut self, feedback: &PadFeedback) -> Result<(), InputError> {
        match *feedback {
            PadFeedback::Rumble(command) => self.rumble(&command),
            PadFeedback::Lightbar {
                slot,
                red,
                green,
                blue,
            } => self.set_leds(slot, |leds, input| {
                write_lightbar(leds, input, [red, green, blue])
            }),
            PadFeedback::PlayerLeds { slot, mask } => {
                self.set_leds(slot, |leds, input| write_player_leds(leds, input, mask))
            }
            PadFeedback::TriggerEffect { .. } => Ok(()),
        }
    }

    fn set_leds(
        &self,
        slot: u8,
        write: impl FnOnce(&Path, &str) -> io::Result<()>,
    ) -> Result<(), InputError> {
        let (Some(leds), Some(sysfs)) = (&self.leds, &self.sysfs) else {
            return Ok(());
        };
        let Some(input) = self
            .pads
            .iter()
            .find(|pad| pad.slot == slot)
            .and_then(|pad| input_name(sysfs, &pad.path))
        else {
            return Ok(());
        };

        write(leds, &input).map_err(|source| InputError::PhysicalGamepadLed { slot, source })
    }

    pub fn disconnect_all(&mut self, out: &mut Vec<InputEvent>) {
        while !self.pads.is_empty() {
            self.remove(self.pads.len() - 1, out);
        }
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<Source>, InputError> {
        let mut sources = Vec::new();
        let mut fds = Vec::new();
        for (pad_index, pad) in self.pads.iter().enumerate() {
            sources.push(Source {
                pad: pad_index,
                sibling: None,
            });
            fds.push(PollFd::new(&pad.device, PollFlags::IN));

            for (sibling_index, sibling) in pad.siblings.iter().enumerate() {
                sources.push(Source {
                    pad: pad_index,
                    sibling: Some(sibling_index),
                });
                fds.push(PollFd::new(&sibling.device, PollFlags::IN));
            }
        }

        let limit = Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };

        match poll(&mut fds, Some(&limit)) {
            Ok(_) => {}
            Err(Errno::INTR) => return Ok(Vec::new()),
            Err(errno) => return Err(InputError::PhysicalGamepadPoll(errno.into())),
        }

        Ok(fds
            .iter()
            .zip(sources)
            .filter(|(fd, _)| !fd.revents().is_empty())
            .map(|(_, source)| source)
            .collect())
    }

    fn remove(&mut self, index: usize, out: &mut Vec<InputEvent>) {
        let pad = self.pads.remove(index);
        self.slots.release(&pad.path);

        out.push(InputEvent::Gamepad {
            slot: pad.slot,
            event: GamepadEvent::Disconnected,
        });
    }

    fn remove_sibling(&mut self, pad: usize, index: usize, out: &mut Vec<InputEvent>) {
        let pad = &mut self.pads[pad];
        let mut sibling = pad.siblings.remove(index);

        sibling.release(pad.slot, out);
    }

    fn scan(&mut self, out: &mut Vec<InputEvent>) {
        let present = event_nodes(&self.root);
        self.rejected.retain(|path| present.contains(path));
        self.known.retain(|path, _| present.contains(path));

        let gone: Vec<usize> = self
            .pads
            .iter()
            .enumerate()
            .filter(|(_, pad)| !present.contains(&pad.path))
            .map(|(index, _)| index)
            .collect();
        for index in gone.into_iter().rev() {
            self.remove(index, out);
        }
        self.drop_vanished_siblings(&present, out);

        for path in present {
            if self.rejected.contains(&path) {
                continue;
            }

            let settled = self.known.get(&path).is_some_and(|node| {
                node.role != NodeRole::Pad || self.slots.slot_of(&path).is_some()
            });
            if settled {
                continue;
            }

            let Ok(device) = Device::open(&path) else {
                continue;
            };
            let Some(description) = describe(&device, &path, self.sysfs.as_deref()) else {
                self.rejected.insert(path);
                continue;
            };
            let role = description.role;
            self.known.insert(path.clone(), description);

            if role == NodeRole::Pad && !self.connect(path, device, out) {
                break;
            }
        }

        self.attach_siblings();
    }

    fn connect(&mut self, path: PathBuf, device: Device, out: &mut Vec<InputEvent>) -> bool {
        let Some(caps) = gamepad_caps(&device) else {
            self.rejected.insert(path);
            return true;
        };
        let Some(slot) = self.slots.assign(path.clone()) else {
            return false;
        };
        if device.set_nonblocking(true).is_err() {
            self.slots.release(&path);
            return true;
        }

        let rumble_capable = device
            .supported_ff()
            .is_some_and(|ff| ff.contains(FFEffectCode::FF_RUMBLE));
        self.pads.push(Pad {
            path,
            slot,
            device,
            translator: PadTranslator::new(caps, self.options.deadzone),
            rumble_capable,
            effect: None,
            siblings: Vec::new(),
        });
        out.push(InputEvent::Gamepad {
            slot,
            event: GamepadEvent::Connected,
        });

        true
    }

    fn drop_vanished_siblings(&mut self, present: &BTreeSet<PathBuf>, out: &mut Vec<InputEvent>) {
        for pad in 0..self.pads.len() {
            let vanished: Vec<usize> = self.pads[pad]
                .siblings
                .iter()
                .enumerate()
                .filter(|(_, sibling)| !present.contains(&sibling.path))
                .map(|(index, _)| index)
                .collect();

            for index in vanished.into_iter().rev() {
                self.remove_sibling(pad, index, out);
            }
        }
    }

    fn attach_siblings(&mut self) {
        let nodes: Vec<NodeDescription> = self.known.values().cloned().collect();

        for pairing in pair_siblings(&nodes) {
            let Some(pad) = self.pads.iter_mut().find(|pad| pad.path == pairing.pad) else {
                continue;
            };

            for (role, path) in [
                (NodeRole::Motion, pairing.motion),
                (NodeRole::Touchpad, pairing.touchpad),
            ] {
                let Some(path) = path.filter(|_| !pad.has_sibling(role)) else {
                    continue;
                };
                if let Some(sibling) = open_sibling(path, role) {
                    pad.siblings.push(sibling);
                }
            }
        }
    }
}

fn event_nodes(root: &Path) -> BTreeSet<PathBuf> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(EVENT_PREFIX)
        })
        .map(|entry| entry.path())
        .collect()
}

fn describe(device: &Device, path: &Path, sysfs: Option<&Path>) -> Option<NodeDescription> {
    let caps = NodeCaps {
        south_button: device
            .supported_keys()
            .is_some_and(|keys| keys.contains(KeyCode::BTN_SOUTH)),
        accelerometer: device.properties().contains(PropType::ACCELEROMETER),
        abs_axes: device
            .supported_absolute_axes()
            .map(|axes| axes.iter().map(|axis| axis.0).collect())
            .unwrap_or_default(),
    };
    let role = caps.role()?;

    let id = device.input_id();
    Some(NodeDescription {
        path: path.to_path_buf(),
        role,
        identity: NodeIdentity {
            vendor: id.vendor(),
            product: id.product(),
            uniq: non_empty(device.unique_name()),
            phys: non_empty(device.physical_path()),
            parent: sysfs.and_then(|sysfs| sysfs_parent(sysfs, path)),
        },
    })
}

fn sysfs_parent(sysfs: &Path, node: &Path) -> Option<PathBuf> {
    let input_device = fs::canonicalize(sysfs.join(node.file_name()?).join("device")).ok()?;

    input_device
        .parent()
        .filter(|parent| *parent != Path::new(SYSFS_UINPUT_PARENT))
        .map(Path::to_path_buf)
}

fn input_name(sysfs: &Path, node: &Path) -> Option<String> {
    let input_device = fs::canonicalize(sysfs.join(node.file_name()?).join("device")).ok()?;

    Some(input_device.file_name()?.to_string_lossy().into_owned())
}

fn led_write(directory: &Path, file: &str, value: &str) -> io::Result<()> {
    fs::write(directory.join(file), value)
}

fn write_lightbar(leds: &Path, input: &str, [red, green, blue]: [u8; 3]) -> io::Result<()> {
    let directory = leds.join(format!("{input}:{LIGHTBAR_SUFFIX}"));
    if !directory.is_dir() {
        return Ok(());
    }

    let lit = red | green | blue != 0;
    led_write(
        &directory,
        "multi_intensity",
        &format!("{red} {green} {blue}"),
    )?;
    led_write(
        &directory,
        "brightness",
        if lit { LED_MAX_BRIGHTNESS } else { "0" },
    )
}

fn write_player_leds(leds: &Path, input: &str, mask: u8) -> io::Result<()> {
    let mut result = Ok(());

    for index in 0..PLAYER_LED_COUNT {
        let directory = leds.join(format!("{input}:{PLAYER_LED_SUFFIX}{}", index + 1));
        if directory.is_dir() {
            let value = if mask & (1 << index) != 0 { "1" } else { "0" };
            result = result.and(led_write(&directory, "brightness", value));
        }
    }

    result
}

fn open_sibling(path: PathBuf, role: NodeRole) -> Option<Sibling> {
    let device = Device::open(&path).ok()?;
    let translator = match role {
        NodeRole::Motion => SiblingTranslator::Motion(MotionTranslator::new(motion_caps(&device))),
        NodeRole::Touchpad => {
            let (x, y) = touchpad_ranges(&device)?;
            SiblingTranslator::Touchpad(TouchpadTranslator::new(x, y))
        }
        NodeRole::Pad => return None,
    };
    device.set_nonblocking(true).ok()?;

    Some(Sibling {
        path,
        role,
        device,
        translator,
    })
}

fn absinfo(device: &Device, axis: AbsoluteAxisCode) -> Option<evdev::AbsInfo> {
    device
        .get_absinfo()
        .ok()?
        .find_map(|(code, info)| (code == axis).then_some(info))
}

fn motion_caps(device: &Device) -> MotionCaps {
    let resolution = |first: AbsoluteAxisCode, others: [AbsoluteAxisCode; 2]| {
        let present = device.supported_absolute_axes().is_some_and(|axes| {
            axes.contains(first) && others.iter().all(|axis| axes.contains(*axis))
        });

        present
            .then(|| absinfo(device, first))
            .flatten()
            .and_then(|info| u32::try_from(info.resolution()).ok())
    };

    MotionCaps {
        accel_units_per_g: resolution(
            AbsoluteAxisCode::ABS_X,
            [AbsoluteAxisCode::ABS_Y, AbsoluteAxisCode::ABS_Z],
        ),
        gyro_units_per_degree_second: resolution(
            AbsoluteAxisCode::ABS_RX,
            [AbsoluteAxisCode::ABS_RY, AbsoluteAxisCode::ABS_RZ],
        ),
    }
}

fn touchpad_ranges(device: &Device) -> Option<(TouchpadRange, TouchpadRange)> {
    let range = |axis| {
        absinfo(device, axis).map(|info| TouchpadRange {
            min: info.minimum(),
            max: info.maximum(),
        })
    };

    Some((
        range(AbsoluteAxisCode::ABS_MT_POSITION_X)?,
        range(AbsoluteAxisCode::ABS_MT_POSITION_Y)?,
    ))
}

fn gamepad_caps(device: &Device) -> Option<PadCaps> {
    let keys = device.supported_keys()?;
    let axes = device.supported_absolute_axes()?;
    let is_gamepad = keys.contains(KeyCode::BTN_SOUTH)
        && axes.contains(AbsoluteAxisCode::ABS_X)
        && axes.contains(AbsoluteAxisCode::ABS_Y);
    if !is_gamepad {
        return None;
    }

    let layout = if device.input_id().vendor() == VENDOR_SONY {
        FaceLayout::Positional
    } else {
        FaceLayout::Xpad
    };
    let ranges = device
        .get_absinfo()
        .ok()?
        .map(|(code, info)| {
            (
                code.0,
                RawAxis {
                    min: info.minimum(),
                    max: info.maximum(),
                },
            )
        })
        .collect();

    Some(PadCaps {
        layout,
        axes: ranges,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::symlink,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    use evdev::{AbsoluteAxisCode, KeyCode};

    use super::*;
    use crate::{
        GamepadAxis, GamepadButton,
        gamepad::{Report, UinputGamepad},
    };

    fn options() -> PhysicalOptions {
        PhysicalOptions {
            max_gamepads: 4,
            deadzone: 0.0,
            rumble_max_ms: 5000,
        }
    }

    fn event_node(gamepad: &mut UinputGamepad) -> PathBuf {
        gamepad
            .device_mut()
            .enumerate_dev_nodes_blocking()
            .unwrap()
            .filter_map(Result::ok)
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(EVENT_PREFIX))
            })
            .unwrap()
    }

    fn poll_until(
        pads: &mut PhysicalGamepads,
        out: &mut Vec<InputEvent>,
        done: impl Fn(&[InputEvent]) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(out) && Instant::now() < deadline {
            pads.poll(Duration::from_millis(20), out).unwrap();
        }
    }

    #[test]
    fn an_empty_directory_has_no_pads() {
        let directory = tempfile::tempdir().unwrap();
        let mut pads = PhysicalGamepads::in_directory(options(), directory.path());
        let mut out = Vec::new();

        pads.poll(Duration::from_millis(1), &mut out).unwrap();

        assert_eq!(out, Vec::new());
        assert_eq!(pads.connected(), 0);
    }

    fn led_tree(directory: &Path, names: &[&str]) -> PathBuf {
        let leds = directory.join("leds");
        for name in names {
            fs::create_dir_all(leds.join(name)).unwrap();
        }

        leds
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn the_input_name_is_the_directory_the_device_link_points_at() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("hid/input/input9");
        fs::create_dir_all(&input).unwrap();
        let class = directory.path().join("class");
        fs::create_dir_all(class.join("event7")).unwrap();
        symlink(&input, class.join("event7/device")).unwrap();

        assert_eq!(
            input_name(&class, Path::new("/dev/input/event7")),
            Some("input9".to_owned())
        );
        assert_eq!(input_name(&class, Path::new("/dev/input/event8")), None);
    }

    #[test]
    fn the_lightbar_writes_its_colour_then_its_brightness() {
        let directory = tempfile::tempdir().unwrap();
        let leds = led_tree(directory.path(), &["input9:rgb:indicator"]);
        let bar = leds.join("input9:rgb:indicator");

        write_lightbar(&leds, "input9", [1, 20, 255]).unwrap();
        assert_eq!(read(&bar.join("multi_intensity")), "1 20 255");
        assert_eq!(read(&bar.join("brightness")), "255");

        write_lightbar(&leds, "input9", [0, 0, 0]).unwrap();
        assert_eq!(read(&bar.join("multi_intensity")), "0 0 0");
        assert_eq!(read(&bar.join("brightness")), "0");
    }

    #[test]
    fn a_pad_without_a_lightbar_or_player_leds_drops_them_without_error() {
        let directory = tempfile::tempdir().unwrap();
        let leds = led_tree(directory.path(), &["input3:rgb:indicator"]);

        assert!(write_lightbar(&leds, "input9", [1, 2, 3]).is_ok());
        assert!(write_player_leds(&leds, "input9", 0x1f).is_ok());
        assert!(!leds.join("input9:rgb:indicator").exists());
    }

    #[test]
    fn player_leds_follow_the_mask_bit_for_bit() {
        let directory = tempfile::tempdir().unwrap();
        let names: Vec<String> = (1..=5)
            .map(|n| format!("input9:white:player-{n}"))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let leds = led_tree(directory.path(), &names);

        write_player_leds(&leds, "input9", 0b10101).unwrap();

        let lit: Vec<String> = names
            .iter()
            .map(|name| read(&leds.join(name).join("brightness")))
            .collect();
        assert_eq!(lit, ["1", "0", "1", "0", "1"]);
    }

    #[test]
    fn a_failed_led_write_is_reported_after_the_other_leds_were_tried() {
        let directory = tempfile::tempdir().unwrap();
        let leds = led_tree(directory.path(), &["input9:white:player-2"]);
        fs::create_dir_all(leds.join("input9:white:player-1/brightness")).unwrap();

        let result = write_player_leds(&leds, "input9", 0b11);

        assert!(result.is_err());
        assert_eq!(read(&leds.join("input9:white:player-2/brightness")), "1");
    }

    #[test]
    fn trigger_effects_and_feedback_for_an_unknown_slot_are_dropped_without_error() {
        let directory = tempfile::tempdir().unwrap();
        let mut pads = PhysicalGamepads::in_directory(options(), directory.path())
            .with_leds_root(directory.path());

        let trigger = pads.apply(&PadFeedback::TriggerEffect {
            slot: 0,
            right: true,
            effect: [0; 11],
        });
        let lightbar = pads.apply(&PadFeedback::Lightbar {
            slot: 2,
            red: 1,
            green: 2,
            blue: 3,
        });
        let leds = pads.apply(&PadFeedback::PlayerLeds { slot: 2, mask: 1 });
        let rumble = pads.apply(&PadFeedback::Rumble(RumbleCommand::stop(2)));

        assert!(trigger.is_ok() && lightbar.is_ok() && leds.is_ok() && rumble.is_ok());
    }

    #[test]
    fn the_sysfs_parent_is_the_directory_holding_the_input_device() {
        let directory = tempfile::tempdir().unwrap();
        let hid = directory.path().join("devices/hid0");
        let input = hid.join("input3");
        let class = directory.path().join("class");
        fs::create_dir_all(&input).unwrap();
        fs::create_dir_all(class.join("event9")).unwrap();
        symlink(&input, class.join("event9/device")).unwrap();

        let parent = sysfs_parent(&class, Path::new("/dev/input/event9"));
        let missing = sysfs_parent(&class, Path::new("/dev/input/event10"));

        assert_eq!(parent, Some(fs::canonicalize(hid).unwrap()));
        assert_eq!(missing, None);
    }

    #[test]
    fn rumble_for_an_unknown_slot_is_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let mut pads = PhysicalGamepads::in_directory(options(), directory.path());

        assert!(pads.rumble(&RumbleCommand::stop(3)).is_ok());
    }

    #[test]
    #[ignore = "needs write access to /dev/uinput and /dev/input"]
    fn live_virtual_pad_round_trips_input_and_rumble() {
        let mut host = UinputGamepad::create(0).unwrap();
        let node = event_node(&mut host);
        let directory = tempfile::tempdir().unwrap();
        symlink(&node, directory.path().join("event7")).unwrap();
        let mut pads = PhysicalGamepads::in_directory(options(), directory.path());
        let mut events = Vec::new();

        poll_until(&mut pads, &mut events, |events| !events.is_empty());
        host.emit(Report::Key {
            code: KeyCode::BTN_SOUTH.0,
            value: 1,
        })
        .unwrap();
        host.emit(Report::Abs {
            code: AbsoluteAxisCode::ABS_X.0,
            value: 32767,
        })
        .unwrap();
        poll_until(&mut pads, &mut events, |events| events.len() >= 3);

        assert_eq!(
            events,
            vec![
                InputEvent::Gamepad {
                    slot: 0,
                    event: GamepadEvent::Connected
                },
                InputEvent::Gamepad {
                    slot: 0,
                    event: GamepadEvent::Button {
                        button: GamepadButton::SOUTH,
                        pressed: true
                    }
                },
                InputEvent::Gamepad {
                    slot: 0,
                    event: GamepadEvent::Axis {
                        axis: GamepadAxis::LEFT_X,
                        value: 1.0
                    }
                },
            ]
        );

        let stop = Arc::new(AtomicBool::new(false));
        let serving = {
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                let mut commands = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    host.poll_rumble(&mut commands).unwrap();
                    thread::sleep(Duration::from_millis(2));
                }
                host.poll_rumble(&mut commands).unwrap();
                (host, commands)
            })
        };

        let start = RumbleCommand {
            slot: 0,
            strong: 0xA000,
            weak: 0x3000,
            delay_ms: 0,
            duration_ms: 400,
        };
        pads.rumble(&start).unwrap();
        pads.rumble(&RumbleCommand::stop(0)).unwrap();
        thread::sleep(Duration::from_millis(50));
        stop.store(true, Ordering::Relaxed);
        let (host, commands) = serving.join().unwrap();
        drop(host);

        assert_eq!(commands, vec![start, RumbleCommand::stop(0)]);
    }

    const SIBLING_VENDOR: u16 = 0x045e;
    const SIBLING_PRODUCT: u16 = 0x028e;

    fn sibling_builder(name: &str) -> evdev::uinput::VirtualDeviceBuilder<'_> {
        evdev::uinput::VirtualDevice::builder()
            .unwrap()
            .name(name)
            .input_id(evdev::InputId::new(
                evdev::BusType::BUS_USB,
                SIBLING_VENDOR,
                SIBLING_PRODUCT,
                1,
            ))
    }

    fn abs_setup(
        code: AbsoluteAxisCode,
        min: i32,
        max: i32,
        resolution: i32,
    ) -> evdev::UinputAbsSetup {
        evdev::UinputAbsSetup::new(code, evdev::AbsInfo::new(0, min, max, 0, 0, resolution))
    }

    fn readable_soon(node: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if Device::open(node).is_ok() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }

        false
    }

    fn first_node(device: &mut evdev::uinput::VirtualDevice) -> PathBuf {
        device
            .enumerate_dev_nodes_blocking()
            .unwrap()
            .filter_map(Result::ok)
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(EVENT_PREFIX))
            })
            .unwrap()
    }

    fn virtual_motion() -> evdev::uinput::VirtualDevice {
        let mut properties = evdev::AttributeSet::<PropType>::new();
        properties.insert(PropType::ACCELEROMETER);
        let mut builder = sibling_builder("rusplay test motion")
            .with_properties(&properties)
            .unwrap();
        for (axis, resolution) in [
            (AbsoluteAxisCode::ABS_X, 8192),
            (AbsoluteAxisCode::ABS_Y, 8192),
            (AbsoluteAxisCode::ABS_Z, 8192),
            (AbsoluteAxisCode::ABS_RX, 1024),
            (AbsoluteAxisCode::ABS_RY, 1024),
            (AbsoluteAxisCode::ABS_RZ, 1024),
        ] {
            builder = builder
                .with_absolute_axis(&abs_setup(axis, -32768, 32767, resolution))
                .unwrap();
        }

        builder.build().unwrap()
    }

    fn virtual_touchpad() -> evdev::uinput::VirtualDevice {
        let mut keys = evdev::AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::BTN_LEFT);
        let mut builder = sibling_builder("rusplay test touchpad")
            .with_keys(&keys)
            .unwrap();
        for (axis, max) in [
            (AbsoluteAxisCode::ABS_MT_SLOT, 1),
            (AbsoluteAxisCode::ABS_MT_TRACKING_ID, 65535),
            (AbsoluteAxisCode::ABS_MT_POSITION_X, 1000),
            (AbsoluteAxisCode::ABS_MT_POSITION_Y, 1000),
        ] {
            builder = builder
                .with_absolute_axis(&abs_setup(axis, 0, max, 0))
                .unwrap();
        }

        builder.build().unwrap()
    }

    #[test]
    #[ignore = "needs write access to /dev/uinput and /dev/input"]
    fn live_motion_and_touchpad_nodes_pair_with_their_pad() {
        let mut pad = UinputGamepad::create(0).unwrap();

        let mut motion = virtual_motion();
        let mut touchpad = virtual_touchpad();

        let directory = tempfile::tempdir().unwrap();
        let nodes = directory.path().join("dev");
        let sysfs = directory.path().join("class");
        let hid = directory.path().join("devices/hid0");
        fs::create_dir_all(&nodes).unwrap();
        for (index, device) in [pad.device_mut(), &mut touchpad, &mut motion]
            .into_iter()
            .enumerate()
        {
            let name = format!("event{}", 10 + index);
            let input = hid.join(format!("input{index}"));
            fs::create_dir_all(&input).unwrap();
            fs::create_dir_all(sysfs.join(&name)).unwrap();
            symlink(&input, sysfs.join(&name).join("device")).unwrap();
            let node = first_node(device);
            if index > 0 && !readable_soon(&node) {
                eprintln!(
                    "skipped live_motion_and_touchpad_nodes_pair_with_their_pad: {} is not readable, sibling nodes need their own uaccess rule",
                    node.display()
                );
                return;
            }
            symlink(node, nodes.join(name)).unwrap();
        }
        let mut pads = PhysicalGamepads::with_roots(options(), &nodes, Some(sysfs));
        let mut events = Vec::new();

        poll_until(&mut pads, &mut events, |events| !events.is_empty());
        let deadline = Instant::now() + Duration::from_secs(5);
        while pads.siblings(0) < 2 && Instant::now() < deadline {
            pads.next_scan = Instant::now();
            pads.poll(Duration::from_millis(20), &mut events).unwrap();
        }

        assert_eq!(pads.siblings(0), 2);

        events.clear();
        let emit = |device: &mut evdev::uinput::VirtualDevice, codes: &[(u16, i32)]| {
            let mut batch: Vec<_> = codes
                .iter()
                .map(|(code, value)| {
                    evdev::InputEvent::new(evdev::EventType::ABSOLUTE.0, *code, *value)
                })
                .collect();
            batch.push(evdev::InputEvent::new(
                evdev::EventType::SYNCHRONIZATION.0,
                0,
                0,
            ));
            device.emit(&batch).unwrap();
        };
        emit(
            &mut motion,
            &[
                (AbsoluteAxisCode::ABS_X.0, 0),
                (AbsoluteAxisCode::ABS_Y.0, 8192),
                (AbsoluteAxisCode::ABS_Z.0, 0),
            ],
        );
        emit(
            &mut touchpad,
            &[
                (AbsoluteAxisCode::ABS_MT_SLOT.0, 0),
                (AbsoluteAxisCode::ABS_MT_TRACKING_ID.0, 5),
                (AbsoluteAxisCode::ABS_MT_POSITION_X.0, 250),
                (AbsoluteAxisCode::ABS_MT_POSITION_Y.0, 750),
            ],
        );
        poll_until(&mut pads, &mut events, |events| events.len() >= 2);

        assert!(events.contains(&InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Sensor {
                sensor: crate::SensorKind::Accel,
                x: 0.0,
                y: 9.80665,
                z: 0.0,
            },
        }));
        assert!(events.contains(&InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::TouchpadDown {
                finger: 0,
                x: 0.25,
                y: 0.75,
            },
        }));
    }
}
