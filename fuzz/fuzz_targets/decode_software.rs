#![no_main]

use std::num::NonZeroU32;

use libfuzzer_sys::fuzz_target;
use rusplay_config::SoftwareDecoderConfig;
use rusplay_core::Codec;
use rusplay_decode::Rav1dDecoder;
use rusplay_fuzz::feed;

fuzz_target!(|data: &[u8]| {
    let config = SoftwareDecoderConfig {
        threads: NonZeroU32::new(1),
    };

    if let Ok(mut decoder) = Rav1dDecoder::new(&config) {
        feed(&mut decoder, Codec::Av1, data);
    }
});
