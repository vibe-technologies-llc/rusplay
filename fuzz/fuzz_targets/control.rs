#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_protocol::{ControlMessage, ControlPacket, StreamKind};

const PAD_FEEDBACK_TAGS: [u8; 3] = [12, 13, 14];

fn round_trips(data: &[u8]) {
    if let Ok(message) = ControlMessage::decode(data) {
        let mut encoded = [0; ControlMessage::MAX_ENCODED_LEN];
        let length = message.encode(&mut encoded).unwrap();

        assert_eq!(
            ControlMessage::decode(&encoded[..length]).ok(),
            Some(message)
        );
    }
}

fuzz_target!(|data: &[u8]| {
    round_trips(data);

    for tag in PAD_FEEDBACK_TAGS {
        let mut tagged = vec![tag];
        tagged.extend_from_slice(data);
        round_trips(&tagged);
    }

    let _ = ControlPacket::read(data);
    let _ = ControlPacket::read(&stamped(data, StreamKind::Control));
});
