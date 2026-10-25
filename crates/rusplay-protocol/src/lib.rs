mod control;
mod cursor;
mod discovery;
mod error;
mod fec;
mod header;
mod input;
mod pairing;
mod reconnect;

pub use crate::{
    control::{ControlMessage, ControlPacket},
    cursor::{CursorImage, CursorMessage},
    discovery::DiscoveryMessage,
    error::ProtocolError,
    fec::FecHeader,
    header::{PacketFlags, PacketHeader, ProtocolVersion, StreamKind},
    input::{InputAdmission, InputPacket, InputRecords, InputSequence, push_input_record},
    pairing::{PairingMessage, PairingRejection},
    reconnect::{PeerId, ReconnectMessage, ReconnectRejection},
};
