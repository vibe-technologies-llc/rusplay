#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_protocol::{ControlMessage, ControlPacket, StreamKind};

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = ControlMessage::decode(data) {
        let mut encoded = [0; ControlMessage::MAX_ENCODED_LEN];
        let length = message.encode(&mut encoded).unwrap();

        assert_eq!(
            ControlMessage::decode(&encoded[..length]).ok(),
            Some(message)
        );
    }
    let _ = ControlPacket::read(data);
    let _ = ControlPacket::read(&stamped(data, StreamKind::Control));
});
