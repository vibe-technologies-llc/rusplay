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
- Mono and stereo use one `OpusEncoder`/`OpusDecoder`. 5.1 and 7.1 are multistream: opus-rs has
  only a multistream decoder, so `multistream.rs` builds the encoder from one `OpusEncoder` per
  elementary stream and joins the packets in RFC 6716 Appendix B self-delimited framing (all but
  the last), which opus-rs's `MultistreamDecoder` splits again. Streams follow mapping family 1
  (coupled front pair, coupled rear/side pairs, centre, LFE); the total `bitrate_kbps` is split
  per channel across streams. PCM in and out is in PipeWire/WAVE channel order (FL FR FC LFE RL RR
  [SL SR]), so `mapping_table` is the family 1 table permuted to that order, not the Vorbis order.
- opus-rs uses more than the default 2 MiB thread stack in unoptimised builds. Codec work runs on
  threads spawned with `CODEC_THREAD_STACK_BYTES`, and tests that touch the codec do the same.
- Capture asks PipeWire for 48 kHz float. If resampling is ever needed (a 96 kHz FLAC stream, a
  device that will not negotiate), it runs on the CPU with `rubato`, or with the resampler from
  the author's Resonate project, never through a C library.
