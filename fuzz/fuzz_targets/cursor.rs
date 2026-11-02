#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_protocol::CursorMessage;

fuzz_target!(|data: &[u8]| {
    let _ = CursorMessage::decode(data);
});
