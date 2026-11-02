#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use rusplay_fuzz::{datagram_list, stamped};
use rusplay_net::{Reassembler, ReassemblyLimits};
use rusplay_protocol::{PacketHeader, StreamKind};

const STREAMS: [StreamKind; 3] = [StreamKind::Video, StreamKind::Audio, StreamKind::Microphone];
const STEP: Duration = Duration::from_millis(7);

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };

    let stream = STREAMS[usize::from(selector) % STREAMS.len()];
    let mut reassembler = Reassembler::new(stream, ReassemblyLimits::default());
    let mut events = Vec::new();
    let mut now = Instant::now();

    for datagram in datagram_list(rest) {
        let kind = match datagram.first() {
            Some(byte) if byte & 1 == 1 => StreamKind::Fec,
            _ => stream,
        };
        let mut datagram = stamped(datagram, kind);
        if kind == StreamKind::Fec
            && let Some(protects) = datagram.get_mut(PacketHeader::SIZE)
        {
            *protects = stream as u8;
        }

        now += STEP;
        let _ = reassembler.push(&datagram, now, &mut events);
        reassembler.poll_timeouts(now, &mut events);
        events.clear();
    }

    let _ = reassembler.next_timeout();
    let _ = reassembler.stats();
});
