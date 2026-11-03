# Tests
- [M] `test-fuzz-network-parsers`: `fuzz/` covers the network and input decoders and `Reassembler::push`; it still lacks targets for the decoders' bitstream entry points (`decode-av1-obu-framing-panic` shows `cros-codecs` panics there; release builds abort on panic, so a parse panic ends the app)
