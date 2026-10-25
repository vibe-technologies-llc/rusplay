#[cfg(feature = "libei")]
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
    #[error("a pointer value is not a finite number")]
    NonFiniteValue,
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
}
