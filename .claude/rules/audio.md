---
paths:
  - "crates/rusplay-audio/**"
---

# Audio

- Opus through `opus-rs` (pure Rust) is the default codec, at 48 kHz in `RestrictedLowDelay`
  mode with 5 ms frames and in-band FEC. FLAC is behind the `flac` feature, for lossless audio on
  fast LANs.
- FLAC packets are bare frames, one per `FlacConfig::block_size` samples, with no `fLaC` marker
  or STREAMINFO: `flacenc` encodes them with `encode_fixed_size_frame` and `claxon`'s
  `FrameReader` decodes each one standalone, so the decoder needs no state from earlier packets.
  Samples are quantised to 24 bits (`f32 * 2^23`, clamped), which an `f32` holds exactly, so a
  round trip is bit-exact for any 24-bit-representable input. `compression_level` 0..=8 maps to
  stereo-decorrelation and LPC settings in `encoder_settings`; block sizes must be 32..=32767.
- Opus supports mono and stereo today. 5.1/7.1 need opus-rs's multistream encoder and are
  rejected with `OpusLayoutUnsupported` until then.
- opus-rs uses more than the default 2 MiB thread stack in unoptimised builds. Codec work runs on
  threads spawned with `CODEC_THREAD_STACK_BYTES`, and tests that touch the codec do the same.
- Capture asks PipeWire for 48 kHz float. If resampling is ever needed (a 96 kHz FLAC stream, a
  device that will not negotiate), it runs on the CPU with `rubato`, or with the resampler from
  the author's Resonate project, never through a C library.
