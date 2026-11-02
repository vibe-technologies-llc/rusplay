#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_protocol::{ControlMessage, ControlPacket, StreamKind};

fuzz_target!(|data: &[u8]| {
    let _ = ControlMessage::decode(data);
    let _ = ControlPacket::read(data);
    let _ = ControlPacket::read(&stamped(data, StreamKind::Control));
});
