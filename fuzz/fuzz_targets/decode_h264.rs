#![no_main]

use libfuzzer_sys::fuzz_target;
use rusplay_core::Codec;
use rusplay_decode::VaapiDecoder;
use rusplay_fuzz::feed;

fuzz_target!(|data: &[u8]| {
    let mut decoder = VaapiDecoder::detached(Codec::H264);

    feed(&mut decoder, Codec::H264, data);
});
