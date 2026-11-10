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
  `PcmResampler::rate_matching` is the playback variant: it always resamples, even at equal
  rates, so `set_speed` can bend the ratio by parts per million (clamped to the widest speed it
  was built for, ramped over the next chunk). Its kernel is 64 taps, Hann² window, cubic
  interpolation, chosen by measurement: flat to 20 kHz (the 64-tap Blackman-Harris² of the
  fixed converter is 6 dB down at 19 kHz, fine for a rate conversion that is rarely needed,
  wrong for a filter every sample passes through), residual below -100 dB, 32 frames of delay.
- PipeWire capture (`AudioCapture`, feature `pipewire`) runs the PipeWire main loop on its own
  thread (`pw_common::PipewireThread`) and connects an input stream. `CaptureTarget::SinkMonitor`
  (`AudioCapture::start`) sets `stream.capture.sink`, so it follows the default sink's monitor;
  `CaptureTarget::Source` (`start_from`) leaves it unset with the `Communication` role, so it
  follows the default source, the microphone. It asks for 32-bit float at the configured rate
  and channel layout, with explicit channel positions (`channel_positions`) and `node.latency`
  set to one codec frame. The `process` callback only converts bytes to `f32` and pushes whole
  frames into an `rtrb` ring (250 ms), converting the mapped bytes straight into the ring's
  write chunk (`RingWriter::write_le_bytes`) so the realtime thread never allocates; a full ring
  drops the newest frames and counts them in `overruns`. It never blocks: the consumer is woken with a `try_lock` and `Thread::unpark`.
  The consumer side (`SampleSource`) drains the ring, runs `PcmResampler` when the negotiated
  rate differs from the configured one, and hands out exact codec frames through
  `read_frame`/`try_read_frame`. Startup waits for the first negotiated format and fails with a
  specific error on a stream error, a channel-count mismatch or a timeout.
- Live PipeWire tests are `#[ignore]`d and run by hand with
  `cargo test -p rusplay-audio --all-features -- --ignored`. `PIPEWIRE_NODE=<sink name>` points
  the capture at another sink, which allows a loopback check against a null sink without sound.
  `PIPEWIRE_NODE=rusplay-microphone` points a `Source` capture at the virtual microphone for the
  `playback` loopback test.
- Playback (`AudioPlayback`, feature `pipewire`) mirrors capture: a PipeWire output stream on its
  own thread whose `process` callback (`Playout::render`) reads an `rtrb` ring and never blocks or
  allocates. The ring holds `max(250 ms, 3 * max(jitter_buffer_max_ms, jitter_buffer_ms))` at
  192 kHz, because it is built before the device rate is known and must fit twice the jitter
  floor at any device rate; `Playout::retarget` stores the negotiated rate, from which the
  callback computes the floor in native-rate frames. `submit` (`Feed`) writes decoded frames
  into the ring through the rate-matching `PcmResampler` (below; a plain one, passthrough at
  equal rates, when `audio.rate_match_max_ppm` is 0), and counts frames dropped when the ring is
  full as `overflows`.
- Clock recovery (`ClockRecovery`): the host's capture clock and the playback device's clock are
  different crystals, so without correction the queue drifts until it underruns or is trimmed,
  an audible glitch every few minutes whatever the codec or buffer size. While playing, each
  callback low-passes the queue level left after its read (1 s) and runs a critically damped PI
  loop on its distance from the jitter target (natural frequency 0.1 rad/s: proportional
  0.2 /s, integral 0.01 /s², both on the error in seconds), which yields a playback speed
  `1 + correction`, clamped to `audio.rate_match_max_ppm` (default 5000, at most 20 000, 0
  turns the loop and the always-on resampler off). The speed is published through an atomic and
  `Feed::submit` applies it with `PcmResampler::set_speed` before each frame, so the realtime
  callback never resamples. The integral is the drift estimate and survives underruns and
  trims; the smoothed level is re-seeded when playback (re)starts or a trim jumps it. The loop
  is deliberately slow: packet-phase sawtooth on the level moves the speed by well under
  0.1 %, a 5 ms target change settles in under a minute. A simulated ±300 ppm drift plays
  ten minutes with no underrun and no trim (`playback_speed_locks_to_…`), and the same link
  without rate matching does not.
- `AudioPlayback::start_to(config, PlaybackTarget::VirtualSource)` publishes the stream as a
  PipeWire `Audio/Source` node (`media.class`, `node.name = rusplay-microphone`) instead of
  autoconnecting to a sink, which is how the host exposes the client's microphone to games. A
  source node only negotiates once something captures it, so startup completes when the stream
  reaches `Paused` and treats the requested rate and channels as the format (nothing converts
  on this side). With no consumer `process` is not called, the ring fills and drops its newest
  frames (`overflows`); the first callback after a consumer appears trims the backlog down to the
  jitter floor, so stale audio never plays.
- The jitter buffer is `audio.jitter_buffer_ms` (default 10, at most 500) of audio kept queued
  *beyond* the quantum the callback is about to play, so it is the lateness the decode side can
  absorb whatever the graph's quantum is. The callback outputs silence until `target + quantum`
  frames are queued, then plays. A quantum that arrives short plays what there is, pads with
  silence, counts an `underrun` and re-buffers up to the target before playing again, so small
  refills under load do not stutter between sound and silence. Neither edge is a hard cut: the
  samples after the last one played decay from it to zero over `DECLICK_SPAN` (2 ms, raised
  cosine, continuing into the next callback if the quantum is shorter), and playback fades in
  over the same span whenever it starts. If the queue grows past twice
  `target + quantum` (a burst after a stall, a consumer appearing on the virtual source) the
  surplus above it is dropped and counted as a `trim`, so latency cannot creep up; anything
  smaller the clock recovery plays off.
- The target adapts (`JitterPolicy`): each underrun raises it by one codec frame up to
  `audio.jitter_buffer_max_ms` (default 50, at most 500, never below `jitter_buffer_ms`; equal
  disables growth), and after `audio.jitter_relax_ms` (default 10 000, 0 never relaxes) of
  playback without an underrun it falls back one codec frame toward `jitter_buffer_ms`, but
  only if the window showed room for it: the callback tracks the deepest dip of the queue level
  below its smoothed value, and the target is lowered only when the lowered target minus that
  dip still leaves half a codec frame. Otherwise the window restarts and the target stays, so a
  link that needs the larger buffer is not probed into an underrun every relax interval.
  Lowering the target drops nothing: the clock recovery plays the surplus off. Only with rate
  matching off does a relax drop up to one codec frame of backlog. Time is counted in rendered
  frames, so the realtime callback makes no clock calls. A drop while audio is playing (a trim,
  or a relax without rate matching) crossfades linearly over the quantum from the audio that
  would have played to the audio after the drop, so it does not click. The ring is sized from the ceiling, and the current target is
  published as `AudioOutputStats::jitter_target_us` (the client's `audio_jitter_target_us`).
- Lost packets are the caller's to conceal before `submit`: `AudioDecoder::conceal` runs Opus
  PLC (`decode` with an empty packet) for mono and stereo, `recover` rebuilds the lost frame from
  the in-band FEC of the packet that followed. opus-rs's multistream decoder has no loss path, so
  surround and FLAC conceal with silence.
- The loss policy is `ConcealingPlayer` (`player.rs`, no PipeWire needed): it tracks the next
  expected packet number, bridges a gap of `n` frames with `conceal` for the first `n - 1` and
  `recover` from the arriving packet for the last, produces at most `audio.max_conceal_ms` worth
  of frames, discards late and duplicate numbers (serial-number arithmetic, so wrap is
  transparent), replaces a decode error with one concealed frame, and submits the results in
  order to an `AudioOutput` (`submit`, `stats`; `AudioPlayback` implements it). Its tallies are
  `PlayerStats`. Both the client's audio stage and the host's microphone stage drive it.
- `AudioInput` (`read_frame`, `overruns`) is the capture seam the client's microphone stage
  reads; `AudioCapture` implements it.
- The client consumes `AudioDecoder`, `AudioPlayback` and `ConcealingPlayer` for the host's audio
  (see `client.md`). The host consumes `AudioCapture` and the encoders from its audio stage (see
  `host.md`): one codec frame per `read_frame`, one packet per frame, numbered from 0 once the
  client is streaming.
- The microphone (client to host) reuses all of it with the derived config
  `AudioConfig::microphone_stream`: Opus, mono, `audio.sample_rate`, with the bitrate, frame
  duration, in-band FEC, jitter buffer and FEC overhead of `audio.microphone`, and the queue,
  concealment, adaptive jitter limits (`jitter_buffer_max_ms`, `jitter_relax_ms`) and rate
  matching (`rate_match_max_ppm`) of the main audio. The client captures with `CaptureTarget::Source`, encodes
  and sends on `StreamKind::Microphone`; the host decodes with `ConcealingPlayer` into an
  `AudioPlayback` on `PlaybackTarget::VirtualSource`. Opus decoding needs the frame duration, so
  both ends must use the same `audio.microphone.frame_duration`.
