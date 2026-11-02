#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_protocol::{PairingMessage, StreamKind};

fuzz_target!(|data: &[u8]| {
    let _ = PairingMessage::from_datagram(data);
    let _ = PairingMessage::from_datagram(&stamped(data, StreamKind::Pairing));
});
