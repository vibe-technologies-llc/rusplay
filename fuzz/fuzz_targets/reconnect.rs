#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_protocol::{ReconnectMessage, StreamKind};

fuzz_target!(|data: &[u8]| {
    let _ = ReconnectMessage::from_datagram(data);
    let _ = ReconnectMessage::from_datagram(&stamped(data, StreamKind::Reconnect));
});
