#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::stamped;
use rusplay_input::{HeldInput, InputEvent, InputRecord};
use rusplay_protocol::{InputPacket, InputRecords, StreamKind};

fn decode_records(section: &[u8]) {
    for record in InputRecords::new(section).flatten() {
        let _ = InputRecord::decode(record);
    }
}

fn apply(held: &mut HeldInput, record: InputRecord) {
    let mut events = Vec::new();
    match record {
        InputRecord::Event(event) => events.push(event),
        InputRecord::State(state) => held.reconcile(&state, &mut events),
    }

    for event in &events {
        held.observe(event);
    }
}

fn round_trips(bytes: &[u8]) {
    if let Ok(record) = InputRecord::decode(bytes) {
        let mut again = Vec::new();
        record.encode_into(&mut again);

        assert_eq!(again, bytes);
        assert_eq!(record.encoded_len(), bytes.len());
    }
}

fuzz_target!(|data: &[u8]| {
    let _ = InputEvent::decode(data);
    let _ = InputRecord::decode(data);
    round_trips(data);
    decode_records(data);

    let mut held = HeldInput::new();
    for record in InputRecords::new(data).flatten() {
        if let Ok(record) = InputRecord::decode(record) {
            apply(&mut held, record);
        }
    }

    let mut snapshot = Vec::new();
    held.snapshot(&mut snapshot);
    let mut mirror = HeldInput::new();
    for record in &snapshot {
        apply(&mut mirror, InputRecord::State(*record));
    }
    let mut release = Vec::new();
    held.release_all(&mut release);

    if let Ok(packet) = InputPacket::read(&stamped(data, StreamKind::Input)) {
        decode_records(packet.current);
        decode_records(packet.previous);
    }
});
