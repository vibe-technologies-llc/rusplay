#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_input::{InputEvent, InputRecord};
use rusplay_protocol::{InputPacket, InputRecords, StreamKind};

fn decode_records(section: &[u8]) {
    for record in InputRecords::new(section).flatten() {
        let _ = InputRecord::decode(record);
    }
}

fuzz_target!(|data: &[u8]| {
    let _ = InputEvent::decode(data);
    let _ = InputRecord::decode(data);
    decode_records(data);

    if let Ok(packet) = InputPacket::read(&stamped(data, StreamKind::Input)) {
        decode_records(packet.current);
        decode_records(packet.previous);
    }
});
