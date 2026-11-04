#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::datagram_list;
use rusplay_protocol::{ClipboardReassembler, ControlMessage};

const CLIPBOARD_TAG: u8 = 20;
const LIMIT: usize = 4096;

fuzz_target!(|data: &[u8]| {
    let mut reassembler = ClipboardReassembler::new(LIMIT);

    for body in datagram_list(data) {
        let mut tagged = vec![CLIPBOARD_TAG];
        tagged.extend_from_slice(body);

        for candidate in [body, tagged.as_slice()] {
            let Ok(message) = ControlMessage::decode(candidate) else {
                continue;
            };

            let mut encoded = [0; ControlMessage::MAX_ENCODED_LEN];
            let length = message.encode(&mut encoded).unwrap();
            assert_eq!(
                ControlMessage::decode(&encoded[..length]).ok(),
                Some(message)
            );

            if let ControlMessage::Clipboard(chunk) = message
                && let Ok(Some(text)) = reassembler.push(&chunk)
            {
                assert!(text.len() <= LIMIT);
            }
        }
    }
});
