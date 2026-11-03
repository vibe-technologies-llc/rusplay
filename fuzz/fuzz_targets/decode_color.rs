#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_core::Codec;
use rusplay_decode::fuzzing::ColorProbe;
use rusplay_fuzz::packets;

const CODECS: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };

    let mut probe = ColorProbe::new(CODECS[usize::from(selector) % CODECS.len()]);

    for packet in packets(rest) {
        let _ = probe.read(packet);
    }
});
