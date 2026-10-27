use std::{
    io,
    os::{fd::OwnedFd, unix::net::UnixStream},
    time::{Duration, Instant},
};

use reis::{
    PendingRequestResult,
    ei::{self, button::ButtonState, keyboard::KeyState},
    enumflags2::BitFlags,
    event::{self, DeviceCapability, EiEvent, EiEventConverter},
    handshake::ei_handshake_blocking,
};
use rusplay_config::InputConfig;
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
    time::{ClockId, clock_gettime},
};

use crate::{
    InputError, InputEvent, InputSink, LockMasks, LockSync,
    libei::{
        AbsoluteMapper, AbsoluteTarget, Capability, EiRequest, MappedRegion, PortalInputOptions,
        Region, RemoteDesktopSession, StartedPortal, translate,
    },
    locks::read_keymap,
};

const MICROS_PER_SECOND: u64 = 1_000_000;
const NANOS_PER_MICRO: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibeiOptions {
    pub name: String,
    pub target: AbsoluteTarget,
}

impl Default for LibeiOptions {
    fn default() -> Self {
        Self {
            name: "rusplay-host".to_owned(),
            target: AbsoluteTarget::default(),
        }
    }
}

struct DeviceSlot {
    device: event::Device,
    resumed: bool,
    emulating: bool,
    absolute: Option<AbsoluteMapper>,
    lock_masks: LockMasks,
}

impl DeviceSlot {
    fn can_emit(&self, capability: Capability) -> bool {
        self.resumed
            && match capability {
                Capability::PointerAbsolute => self.absolute.is_some(),
                other => self.device.has_capability(device_capability(other)),
            }
    }

    fn emit(&self, request: EiRequest) -> Result<(), InputError> {
        let unavailable = || InputError::DeviceUnavailable {
            capability: request.capability(),
        };

        match request {
            EiRequest::Key { code, pressed } => {
                let keyboard = self
                    .device
                    .interface::<ei::Keyboard>()
                    .ok_or_else(unavailable)?;
                keyboard.key(code, key_state(pressed));
            }
            EiRequest::MotionRelative { dx, dy } => {
                let pointer = self
                    .device
                    .interface::<ei::Pointer>()
                    .ok_or_else(unavailable)?;
                pointer.motion_relative(dx, dy);
            }
            EiRequest::MotionNormalised { x, y } => {
                let pointer = self
                    .device
                    .interface::<ei::PointerAbsolute>()
                    .ok_or_else(unavailable)?;
                let mapper = self.absolute.as_ref().ok_or_else(unavailable)?;
                let (x, y) = mapper.map(x, y);
                pointer.motion_absolute(x, y);
            }
            EiRequest::Button { code, pressed } => {
                let button = self
                    .device
                    .interface::<ei::Button>()
                    .ok_or_else(unavailable)?;
                button.button(code, button_state(pressed));
            }
            EiRequest::Scroll120 { dx, dy } => {
                let scroll = self
                    .device
                    .interface::<ei::Scroll>()
                    .ok_or_else(unavailable)?;
                scroll.scroll_discrete(dx, dy);
            }
        }

        Ok(())
    }
}

pub struct LibeiInjector {
    context: ei::Context,
    converter: EiEventConverter,
    options: LibeiOptions,
    bound: BitFlags<DeviceCapability>,
    devices: Vec<DeviceSlot>,
    sequence: u32,
    closed: bool,
    sync_locks: bool,
    lock_sync: LockSync,
    session: Option<RemoteDesktopSession>,
}

impl LibeiInjector {
    pub fn connect(
        fd: OwnedFd,
        config: &InputConfig,
        options: LibeiOptions,
    ) -> Result<Self, InputError> {
        let context = ei::Context::new(UnixStream::from(fd)).map_err(InputError::EiIo)?;
        let handshake =
            ei_handshake_blocking(&context, &options.name, ei::handshake::ContextType::Sender)
                .map_err(|error| InputError::EiProtocol(Box::new(error)))?;

        Ok(Self {
            converter: EiEventConverter::new(&context, handshake),
            context,
            options,
            bound: bound_capabilities(*config),
            devices: Vec::new(),
            sequence: 0,
            closed: false,
            sync_locks: config.sync_locks && config.keyboard,
            lock_sync: LockSync::new(),
            session: None,
        })
    }

    pub async fn open_standalone(
        config: &InputConfig,
        options: LibeiOptions,
    ) -> Result<(Self, StartedPortal), InputError> {
        let session = RemoteDesktopSession::create().await?;
        let started = session
            .start(&PortalInputOptions::from_config(config))
            .await?;
        let fd = session.connect_to_eis().await?;

        let mut injector = Self::connect(fd, config, options)?;
        injector.attach_session(session);

        Ok((injector, started))
    }

    pub fn attach_session(&mut self, session: RemoteDesktopSession) {
        self.session = Some(session);
    }

    pub fn wait_ready(&mut self, timeout: Duration) -> Result<(), InputError> {
        let deadline = Instant::now() + timeout;

        loop {
            self.pump()?;
            if self.devices.iter().any(|slot| slot.resumed) {
                return Ok(());
            }

            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(InputError::DevicesNotResumed)?;
            let limit = Timespec {
                tv_sec: i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: remaining.subsec_nanos().into(),
            };

            let mut fds = [PollFd::new(&self.context, PollFlags::IN)];
            match poll(&mut fds, Some(&limit)) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(errno) => return Err(InputError::EiIo(errno.into())),
            }
        }
    }

    pub fn service(&mut self) -> Result<(), InputError> {
        self.pump()?;
        self.sync_locks()
    }

    pub fn is_ready(&self) -> bool {
        !self.closed && self.devices.iter().any(|slot| slot.resumed)
    }

    fn pump(&mut self) -> Result<(), InputError> {
        match self.context.read() {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                self.closed = true;
                return Err(InputError::SinkClosed);
            }
            Err(error) => return Err(InputError::EiIo(error)),
        }

        while let Some(pending) = self.context.pending_event() {
            match pending {
                PendingRequestResult::Request(request) => self
                    .converter
                    .handle_event(request)
                    .map_err(|error| InputError::EiProtocol(Box::new(error.into())))?,
                PendingRequestResult::ParseError(error) => {
                    return Err(InputError::EiProtocol(Box::new(error.into())));
                }
                PendingRequestResult::InvalidObject(_) => {}
            }
        }

        while let Some(event) = self.converter.next_event() {
            self.apply(event);
        }

        self.flush()
    }

    fn apply(&mut self, event: EiEvent) {
        match event {
            EiEvent::SeatAdded(added) => added.seat.bind_capabilities(self.bound),
            EiEvent::DeviceAdded(added) => {
                let absolute = self.absolute_mapper(&added.device);
                let lock_masks = lock_masks_of(&added.device);
                self.devices.push(DeviceSlot {
                    device: added.device,
                    resumed: false,
                    emulating: false,
                    absolute,
                    lock_masks,
                });
            }
            EiEvent::KeyboardModifiers(modifiers) => {
                if let Some(slot) = self
                    .devices
                    .iter()
                    .find(|slot| slot.device == modifiers.device)
                {
                    let locks = slot.lock_masks.locked(modifiers.locked);
                    self.lock_sync.report(locks);
                }
            }
            EiEvent::DeviceResumed(resumed) => {
                self.update_slot(&resumed.device, true);
            }
            EiEvent::DevicePaused(paused) => {
                self.update_slot(&paused.device, false);
            }
            EiEvent::DeviceRemoved(removed) => {
                self.devices.retain(|slot| slot.device != removed.device);
            }
            EiEvent::Disconnected(_) => self.closed = true,
            _ => {}
        }
    }

    fn update_slot(&mut self, device: &event::Device, resumed: bool) {
        if let Some(slot) = self.devices.iter_mut().find(|slot| &slot.device == device) {
            slot.resumed = resumed;
            slot.emulating = false;
        }
    }

    fn absolute_mapper(&self, device: &event::Device) -> Option<AbsoluteMapper> {
        if !device.has_capability(DeviceCapability::PointerAbsolute) {
            return None;
        }

        let regions: Vec<MappedRegion> = device
            .regions()
            .iter()
            .map(|region| MappedRegion {
                region: Region {
                    x: pixels(region.x),
                    y: pixels(region.y),
                    width: pixels(region.width),
                    height: pixels(region.height),
                },
                mapping_id: region.mapping_id.clone(),
            })
            .collect();

        AbsoluteMapper::new(&regions, &self.options.target).ok()
    }

    fn send(&mut self, request: EiRequest) -> Result<(), InputError> {
        let capability = request.capability();
        let serial = self.converter.connection().serial();
        let timestamp = now_micros();

        let slot = self
            .devices
            .iter_mut()
            .find(|slot| slot.can_emit(capability))
            .ok_or(InputError::DeviceUnavailable { capability })?;

        if !slot.emulating {
            slot.device.device().start_emulating(serial, self.sequence);
            self.sequence = self.sequence.wrapping_add(1);
            slot.emulating = true;
        }

        slot.emit(request)?;
        slot.device.device().frame(serial, timestamp);

        self.flush()
    }

    fn sync_locks(&mut self) -> Result<(), InputError> {
        if !self.sync_locks || self.closed {
            return Ok(());
        }

        let now = Instant::now();
        let taps = self.lock_sync.due(now);
        if taps.is_empty() {
            return Ok(());
        }

        self.lock_sync.tapped(now);
        for code in taps.keycodes() {
            self.send(EiRequest::Key {
                code,
                pressed: true,
            })?;
            self.send(EiRequest::Key {
                code,
                pressed: false,
            })?;
        }

        Ok(())
    }

    fn flush(&self) -> Result<(), InputError> {
        self.context
            .flush()
            .map_err(|errno| InputError::EiIo(errno.into()))
    }
}

impl InputSink for LibeiInjector {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        if let InputEvent::Locks(locks) = event {
            self.pump()?;
            if self.closed {
                return Err(InputError::SinkClosed);
            }

            if self.sync_locks {
                self.lock_sync.desire(locks);
            }

            return self.sync_locks();
        }

        let request = translate(event)?;

        self.pump()?;
        if self.closed {
            return Err(InputError::SinkClosed);
        }

        self.send(request)?;
        if let EiRequest::Key { code, .. } = request {
            self.lock_sync.key_injected(code, Instant::now());
        }

        Ok(())
    }
}

impl Drop for LibeiInjector {
    fn drop(&mut self) {
        let serial = self.converter.connection().serial();
        for slot in self.devices.iter().filter(|slot| slot.emulating) {
            slot.device.device().stop_emulating(serial);
        }
        let _ = self.context.flush();
    }
}

fn bound_capabilities(config: InputConfig) -> BitFlags<DeviceCapability> {
    let mut bound = BitFlags::empty();
    if config.keyboard {
        bound |= DeviceCapability::Keyboard;
    }
    if config.pointer {
        bound |= DeviceCapability::Pointer
            | DeviceCapability::PointerAbsolute
            | DeviceCapability::Button
            | DeviceCapability::Scroll;
    }
    bound
}

fn lock_masks_of(device: &event::Device) -> LockMasks {
    device
        .keymap()
        .filter(|keymap| keymap.type_ == ei::keyboard::KeymapType::Xkb)
        .and_then(|keymap| read_keymap(&keymap.fd, keymap.size))
        .map_or_else(LockMasks::default, |text| LockMasks::from_keymap(&text))
}

fn device_capability(capability: Capability) -> DeviceCapability {
    match capability {
        Capability::Keyboard => DeviceCapability::Keyboard,
        Capability::Pointer => DeviceCapability::Pointer,
        Capability::PointerAbsolute => DeviceCapability::PointerAbsolute,
        Capability::Button => DeviceCapability::Button,
        Capability::Scroll => DeviceCapability::Scroll,
    }
}

fn key_state(pressed: bool) -> KeyState {
    if pressed {
        KeyState::Press
    } else {
        KeyState::Released
    }
}

fn button_state(pressed: bool) -> ButtonState {
    if pressed {
        ButtonState::Press
    } else {
        ButtonState::Released
    }
}

#[allow(clippy::cast_precision_loss)]
fn pixels(value: u32) -> f32 {
    value as f32
}

fn now_micros() -> u64 {
    let now = clock_gettime(ClockId::Monotonic);
    let seconds = u64::try_from(now.tv_sec).unwrap_or_default();
    let nanos = u64::try_from(now.tv_nsec).unwrap_or_default();

    seconds * MICROS_PER_SECOND + nanos / NANOS_PER_MICRO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_capabilities_follow_the_config() {
        let keyboard_only = InputConfig {
            pointer: false,
            ..InputConfig::default()
        };
        let pointer_only = InputConfig {
            keyboard: false,
            ..InputConfig::default()
        };

        assert_eq!(
            bound_capabilities(keyboard_only),
            BitFlags::from(DeviceCapability::Keyboard)
        );
        assert!(!bound_capabilities(pointer_only).contains(DeviceCapability::Keyboard));
        assert!(bound_capabilities(pointer_only).contains(DeviceCapability::PointerAbsolute));
    }

    #[test]
    fn monotonic_clock_moves_forward() {
        let first = now_micros();
        let second = now_micros();

        assert!(first > 0);
        assert!(second >= first);
    }

    #[test]
    fn button_and_key_states_follow_the_flag() {
        assert_eq!(key_state(true), KeyState::Press);
        assert_eq!(key_state(false), KeyState::Released);
        assert_eq!(button_state(true), ButtonState::Press);
        assert_eq!(button_state(false), ButtonState::Released);
    }

    #[tokio::test]
    #[ignore = "needs a Wayland session with the RemoteDesktop portal and a user confirmation"]
    async fn live_portal_session_injects_motion() {
        let config = InputConfig::default();

        let (mut injector, started) =
            LibeiInjector::open_standalone(&config, LibeiOptions::default())
                .await
                .unwrap();
        injector.wait_ready(Duration::from_secs(30)).unwrap();

        assert!(started.pointer);
        injector
            .inject(InputEvent::PointerMotion { dx: 20.0, dy: 0.0 })
            .unwrap();
        injector
            .inject(InputEvent::PointerMotion { dx: -20.0, dy: 0.0 })
            .unwrap();
    }
}
