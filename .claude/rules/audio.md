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
  per channel across streams. PCM in and out is in PipeWire channel order (5.1: FL FR FC LFE SL SR; 7.1: FL FR FC LFE
  RL RR SL SR), so `mapping_table` is the family 1 table permuted to that order, not the Vorbis order.
- opus-rs uses more than the default 2 MiB thread stack in unoptimised builds. Codec work runs on
  threads spawned with `CODEC_THREAD_STACK_BYTES`, and tests that touch the codec do the same.
- Capture asks PipeWire for 48 kHz float. When a device delivers another rate, `PcmResampler`
  converts it on the CPU with `rubato`'s asynchronous sinc resampler (fixed ratio, linear
  interpolation between oversampled sincs, 64-tap filter, 120-frame input chunks, so the delay is
  a few milliseconds). `push` accepts any chunk length and appends whatever whole output chunks
  are ready; equal rates copy straight through. Never a C resampler.
- PipeWire capture (`AudioCapture`, feature `pipewire`) runs the PipeWire main loop on its own
  thread (`pw_common::PipewireThread`) and connects an input stream with `stream.capture.sink`
  set, so it follows the default sink's monitor. It asks for 32-bit float at the configured rate
  and channel layout, with explicit channel positions (`channel_positions`) and `node.latency`
  set to one codec frame. The `process` callback only converts bytes to `f32` and pushes whole
  frames into an `rtrb` ring (250 ms); a full ring drops the newest frames and counts them in
  `overruns`. It never blocks: the consumer is woken with a `try_lock` and `Thread::unpark`.
  The consumer side (`SampleSource`) drains the ring, runs `PcmResampler` when the negotiated
  rate differs from the configured one, and hands out exact codec frames through
  `read_frame`/`try_read_frame`. Startup waits for the first negotiated format and fails with a
  specific error on a stream error, a channel-count mismatch or a timeout.
- Live PipeWire tests are `#[ignore]`d and run by hand with
  `cargo test -p rusplay-audio --all-features -- --ignored`. `PIPEWIRE_NODE=<sink name>` points
  the capture at another sink, which allows a loopback check against a null sink without sound.
- Playback (`AudioPlayback`, feature `pipewire`) mirrors capture: a PipeWire output stream on its
  own thread whose `process` callback (`Playout::render`) reads an `rtrb` ring and never blocks or
  allocates. `submit` writes decoded frames into the ring, through `PcmResampler` if the stream
  negotiated another rate, and counts frames dropped when the ring is full as `overflows`.
- The jitter buffer is `audio.jitter_buffer_ms` (default 10, at most 500). The callback outputs
  silence until at least `max(target, one quantum)` frames are queued, then plays. If it runs dry
  it pads the rest of that quantum with silence, counts an `underrun` and re-buffers up to the
  target. If the queue grows past twice that floor (clock drift, a burst) it drops the surplus
  down to the floor and counts a `trim`, so latency cannot creep up.
- Lost packets are the caller's to conceal before `submit`: `AudioDecoder::conceal` runs Opus
  PLC (`decode` with an empty packet) for mono and stereo, `recover` rebuilds the lost frame from
  the in-band FEC of the packet that followed. opus-rs's multistream decoder has no loss path, so
  surround and FLAC conceal with silence.
