#[cfg(any(feature = "libei", feature = "gamepad", feature = "wayland"))]
use std::io;

use thiserror::Error;

#[cfg(feature = "libei")]
use crate::libei::{AbsoluteTarget, Capability, PortalStep};

#[derive(Debug, Error)]
pub enum InputError {
    #[error("the input sink was closed by the compositor")]
    SinkClosed,
    #[error("gamepad slot {0} is outside the supported range")]
    GamepadSlotOutOfRange(u8),
    #[error("touch slot {0} is outside the supported range")]
    TouchSlotOutOfRange(u8),
    #[error("gamepad slot {slot} has no rumble effect {id}")]
    RumbleEffectUnknown { slot: u8, id: i64 },
    #[error("a pointer value is not a finite number")]
    NonFiniteValue,
    #[error("an encoded input event is missing {missing} bytes")]
    WireTruncated { missing: usize },
    #[error("an encoded input event has {0} trailing bytes")]
    WireTrailingBytes(usize),
    #[error("an encoded input event has the unknown tag {0}")]
    WireUnknownTag(u8),
    #[error("an encoded gamepad event has the unknown tag {0}")]
    WireUnknownGamepadTag(u8),
    #[error("an encoded boolean is {0}, not 0 or 1")]
    WireInvalidBool(u8),
    #[error("an encoded lock state has the unknown bits {0:#04x}")]
    WireInvalidLockBits(u8),
    #[cfg(feature = "libei")]
    #[error("the {step:?} portal call failed")]
    Portal {
        step: PortalStep,
        #[source]
        source: Box<ashpd::Error>,
    },
    #[cfg(feature = "libei")]
    #[error("reading or writing the libei socket failed")]
    EiIo(#[source] io::Error),
    #[cfg(feature = "libei")]
    #[error("the libei connection broke the protocol")]
    EiProtocol(#[source] Box<reis::Error>),
    #[cfg(feature = "libei")]
    #[error("no resumed device offers the {capability:?} capability")]
    DeviceUnavailable { capability: Capability },
    #[cfg(feature = "libei")]
    #[error("the compositor did not resume any input device in time")]
    DevicesNotResumed,
    #[cfg(feature = "libei")]
    #[error("no device region matches the absolute pointer target {target:?}")]
    NoRegion { target: AbsoluteTarget },
    #[cfg(feature = "libei")]
    #[error("gamepad events are not handled by the libei sink")]
    GamepadUnsupported,
    #[cfg(feature = "libei")]
    #[error("a lock state is applied by the injector, not translated to a request")]
    LocksNotARequest,
    #[cfg(feature = "wayland")]
    #[error("the {object} handle does not name a live wayland object")]
    WaylandObjectInvalid { object: &'static str },
    #[cfg(feature = "wayland")]
    #[error("listing the wayland globals failed")]
    WaylandGlobals(#[source] Box<wayland_client::globals::GlobalError>),
    #[cfg(feature = "wayland")]
    #[error("the compositor does not offer {interface}")]
    WaylandGlobalMissing {
        interface: &'static str,
        #[source]
        source: Box<wayland_client::globals::BindError>,
    },
    #[cfg(feature = "wayland")]
    #[error("dispatching wayland events failed")]
    WaylandDispatch(#[source] Box<wayland_client::DispatchError>),
    #[cfg(feature = "wayland")]
    #[error("reading from or flushing to the wayland socket failed")]
    WaylandRead(#[source] Box<wayland_client::backend::WaylandError>),
    #[cfg(feature = "wayland")]
    #[error("waiting on the wayland socket failed")]
    WaylandPoll(#[source] io::Error),
    #[cfg(feature = "wayland")]
    #[error("starting the wayland input thread failed")]
    WaylandSpawn(#[source] io::Error),
    #[cfg(feature = "wayland")]
    #[error("the wayland input thread panicked")]
    WaylandThreadPanicked,
    #[cfg(feature = "gamepad")]
    #[error("gamepad slot {0} has no connected device")]
    GamepadNotConnected(u8),
    #[cfg(feature = "gamepad")]
    #[error("gamepad button {0} is not part of the virtual layout")]
    GamepadButtonUnknown(u16),
    #[cfg(feature = "gamepad")]
    #[error("gamepad axis {0} is not part of the virtual layout")]
    GamepadAxisUnknown(u8),
    #[cfg(feature = "gamepad")]
    #[error("the gamepad sink only handles gamepad events")]
    NonGamepadEvent,
    #[cfg(feature = "gamepad")]
    #[error("creating the uinput device for gamepad slot {slot} failed")]
    UinputCreate {
        slot: u8,
        #[source]
        source: io::Error,
    },
    #[cfg(feature = "gamepad")]
    #[error("reading force feedback requests from the uinput device of gamepad slot {slot} failed")]
    UinputFeedback {
        slot: u8,
        #[source]
        source: io::Error,
    },
    #[cfg(feature = "gamepad")]
    #[error("waiting for physical gamepad events failed")]
    PhysicalGamepadPoll(#[source] io::Error),
    #[cfg(feature = "gamepad")]
    #[error("playing rumble on the physical gamepad in slot {slot} failed")]
    PhysicalGamepadRumble {
        slot: u8,
        #[source]
        source: io::Error,
    },
    #[cfg(feature = "gamepad")]
    #[error("writing to the uinput device of gamepad slot {slot} failed")]
    UinputEmit {
        slot: u8,
        #[source]
        source: io::Error,
    },
}
