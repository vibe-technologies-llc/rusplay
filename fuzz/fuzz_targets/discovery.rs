#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_protocol::{DiscoveryMessage, StreamKind};

fuzz_target!(|data: &[u8]| {
    let _ = DiscoveryMessage::from_datagram(data);
    let _ = DiscoveryMessage::from_datagram(&stamped(data, StreamKind::Discovery));
});
