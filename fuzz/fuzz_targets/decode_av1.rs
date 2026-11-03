#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_core::Codec;
use rusplay_decode::VaapiDecoder;
use rusplay_fuzz::feed;

fuzz_target!(|data: &[u8]| {
    let mut decoder = VaapiDecoder::detached(Codec::Av1);

    feed(&mut decoder, Codec::Av1, data);
});
