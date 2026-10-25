use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use evdev::{
    AbsoluteAxisCode, Device, EventSummary, FFEffect, FFEffectCode, FFEffectData, FFEffectKind,
    FFReplay, FFTrigger, KeyCode,
};
use rusplay_config::InputConfig;
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
};

use crate::{
    GamepadEvent, InputError, InputEvent, RumbleCommand,
    capture::{FaceLayout, PadCaps, PadSlots, PadTranslator, RawAxis, RawEvent},
};

const INPUT_DIRECTORY: &str = "/dev/input";
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

struct Pad {
    path: PathBuf,
    slot: u8,
    device: Device,
    translator: PadTranslator,
    rumble_capable: bool,
    effect: Option<FFEffect>,
}

impl Pad {
    fn read(&mut self, out: &mut Vec<InputEvent>) -> io::Result<()> {
        loop {
            let events: Vec<_> = match self.device.fetch_events() {
                Ok(events) => events.collect(),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            };

            if events.is_empty() {
                return Ok(());
            }

            for event in events {
                let raw = match event.destructure() {
                    EventSummary::Key(_, code, value) => RawEvent::Key {
                        code: code.0,
                        value,
                    },
                    EventSummary::AbsoluteAxis(_, code, value) => RawEvent::Abs {
                        code: code.0,
                        value,
                    },
                    _ => continue,
                };
                self.translator.translate(self.slot, raw, out);
            }
        }
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
    pads: Vec<Pad>,
    slots: PadSlots<PathBuf>,
    rejected: BTreeSet<PathBuf>,
    next_scan: Instant,
}

impl PhysicalGamepads {
    pub fn new(options: PhysicalOptions) -> Self {
        Self::in_directory(options, INPUT_DIRECTORY)
    }

    pub fn in_directory(options: PhysicalOptions, root: impl Into<PathBuf>) -> Self {
        Self {
            options,
            root: root.into(),
            pads: Vec::new(),
            slots: PadSlots::new(options.max_gamepads),
            rejected: BTreeSet::new(),
            next_scan: Instant::now(),
        }
    }

    pub fn connected(&self) -> usize {
        self.pads.len()
    }

    pub fn poll(&mut self, timeout: Duration, out: &mut Vec<InputEvent>) -> Result<(), InputError> {
        let now = Instant::now();
        if now >= self.next_scan {
            self.scan(out);
            self.next_scan = now + SCAN_INTERVAL;
        }

        let wait = timeout.min(self.next_scan.saturating_duration_since(now));
        let ready = self.wait(wait)?;

        let mut gone = Vec::new();
        for index in ready {
            if self.pads[index].read(out).is_err() {
                gone.push(index);
            }
        }

        for index in gone.into_iter().rev() {
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

    pub fn disconnect_all(&mut self, out: &mut Vec<InputEvent>) {
        while !self.pads.is_empty() {
            self.remove(self.pads.len() - 1, out);
        }
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<usize>, InputError> {
        let mut fds: Vec<_> = self
            .pads
            .iter()
            .map(|pad| PollFd::new(&pad.device, PollFlags::IN))
            .collect();
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
            .enumerate()
            .filter(|(_, fd)| !fd.revents().is_empty())
            .map(|(index, _)| index)
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

    fn scan(&mut self, out: &mut Vec<InputEvent>) {
        let present = event_nodes(&self.root);
        self.rejected.retain(|path| present.contains(path));

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

        for path in present {
            if self.rejected.contains(&path) || self.slots.slot_of(&path).is_some() {
                continue;
            }

            let Ok(device) = Device::open(&path) else {
                continue;
            };
            let Some(caps) = gamepad_caps(&device) else {
                self.rejected.insert(path);
                continue;
            };
            let Some(slot) = self.slots.assign(path.clone()) else {
                break;
            };
            if device.set_nonblocking(true).is_err() {
                self.slots.release(&path);
                continue;
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
            });
            out.push(InputEvent::Gamepad {
                slot,
                event: GamepadEvent::Connected,
            });
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
        gamepad::{GamepadDevice, Report, UinputGamepad},
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
}
