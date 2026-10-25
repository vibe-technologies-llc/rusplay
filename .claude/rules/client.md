---
paths:
  - "crates/rusplay-client/**"
---

# Client session

- `ClientSession::connect(host, &Credentials, Gpu, sink)` binds a `Transport` (the configured
  `network.bind_address` when its family matches the host, else the unspecified address, port 0),
  authenticates on the calling thread (`Credentials::Pin` runs `pair_as_client` and exposes the new
  `PeerSecret` through `ClientStream::paired_secret` for the caller to remember;
  `Credentials::Peer` runs `reconnect_as_client`), installs the session keys and only then starts
  the stages, so nothing streams unsealed. It returns once the host has answered the client's
  `Hello`, or fails with `HandshakeTimeout` after `network.handshake_timeout_ms`,
  `HelloUndeliverable` or `HandshakeAborted`. `probe` only opens the configured decoders; it is what
  the app calls until `app-stream-view` replaces it with `connect`.
- Handshake contract the host side must follow: the client sends `ControlMessage::Hello` on the
  control channel right after keys are installed, the host answers with its own `Hello` and starts
  sending video. A host `Goodbye` ends the stream cleanly; the client sends `Goodbye` on `stop`.
- Three named threads (`rusplay-client-receive`, `-decode`, `-present`), plus `-audio` when an
  audio output exists, each returning `Result<(), ClientError>`. Any stage that ends sets the
  shared stop flag; `ClientStream::wait` and `stop` join them all and return the first error.
  Dropping the stream stops it.
- Receive owns the transport, the `ControlChannel` and one `Reassembler` per media stream (video,
  cursor, and audio when there is an output). It waits in
  `Transport::recv_batch_timeout` until the nearest control deadline, reassembly timeout, keyframe
  retry or peer deadline, at most 10 ms, so decode feedback is picked up without a wake-up socket.
  Datagrams from any address but the host are ignored; FEC packets are routed by
  `FecHeader::protects`.
- Feedback, all over the control channel, never blocking the frame path: `PacketsMissing` becomes
  `ReportLoss` (dropped and counted when the control window is full, since reports are advisory);
  a `FrameLost` newer than the last keyframe, a gap in completed frame numbers, a frame dropped
  because the decode queue is full, or a decode error on a non-keyframe enters resync: one
  `RequestKeyframe { frame }` naming the first missing frame, repeated every
  `network.keyframe_request_interval_ms` until a keyframe arrives, and every non-keyframe until
  then is skipped (counted in `frames_skipped`). A decode error on a keyframe is fatal. Nothing
  arriving for `network.peer_timeout_ms` ends the stream with `HostSilent`.
- Receive → decode is a bounded `sync_channel` of `video.decode_queue_frames` encoded frames,
  filled with `try_send`: encoded frames cannot be dropped silently because later frames reference
  them, so overflow drops the frame and resyncs. Decode → present is a latest-wins `Slot`: a
  decoded frame that is superseded before the GPU took it is dropped (`frames_superseded`), which
  also releases a DMA-BUF lease at once. The decoder is opened on the decode thread (decoders are
  not `Send`), whose readiness `connect` waits for.
- Encoded frames carry the receive time since stream start as their `Timestamp`, since video
  packets have no timestamp field. `PresentedFrame` has `received_at`, `decoded_at` and
  `submitted_at` instants for latency accounting.
- Present runs on the app's `Gpu` (device and queue): CPU planes are written row by row into the
  `RgbConverter` upload buffer through `Queue::write_buffer_with` (the one unavoidable copy for
  software decode), unpacked to `rgba16float` at the source resolution and, when the target set by
  `ClientStream::set_target` differs, resampled by `Scaler` (the upscale filter, anti-ringing and
  sigmoid when enlarging either axis, the downscale filter otherwise). Without a target, or at the
  same size, the unpack writes straight into the output texture. Chroma siting is `Left`, matching
  the host's `ColorConverter` default.
- Output textures are `rgba16float` with `TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC`, pooled up
  to four per target size as `Arc<wgpu::Texture>`; a texture is reused once only the pool holds it,
  so the app may keep a `PresentedFrame` for as long as it displays it. If all four are held the
  frame is skipped (`frames_unpresented`). The app's later submissions on the same queue are
  ordered after the present stage's, so no fence is needed.
- The hand-off is `FrameSink` (`present(&mut self, PresentedFrame)` on the present thread, also
  implemented for `FnMut(PresentedFrame)` closures) and `LatestFrame`, a cloneable latest-wins
  sink with `take` and `wait`. Sinks must not block.
- Every `PresentedFrame` carries `FrameColor`: the `ColorSpace` (primaries, transfer, range,
  matrix and the `HdrLevels` with optional mastering and content light metadata, taken from the
  client's own `video.color` until `session-stream-format` carries the host's), the resolved
  `MatrixCoefficients`, the coded `PixelFormat` and the `OutputLight` of the texture. SDR
  transfers (sRGB, BT.709, gamma 2.2, linear) are linearised (`OutputLight::Linear`), PQ and
  HLG are left encoded (`OutputLight::Encoded`, no sigmoid) in the stream's own primaries, so
  nothing assumes an sRGB or BT.709 display. The colour describes what the texture holds: a
  gamut or tone-mapping pass goes in `present.rs` between the unpack and the scaler, on the
  source-resolution linear texture, and updates `FrameColor` accordingly, without changing the
  public types.
- Audio (`audio.rs`): `connect` opens an `AudioOutput` before it spawns the stages. The default
  (`default_audio_output`) is `AudioPlayback` when the `pipewire` feature is on and none
  otherwise; `connect_with_audio` takes any `Option<Box<dyn AudioOutput>>` (tests pass a capturing
  output, `None` skips audio). With no output, `StreamKind::Audio` datagrams are ignored and never
  reassembled. A failure to start the output fails `connect`; a later `submit` failure ends the
  audio stage and with it the stream.
- Host contract for audio: one codec packet per audio frame number, counting up by one, sent
  through a `Packetizer` on `StreamKind::Audio` (so a packet bigger than a datagram, like FLAC,
  fragments and may carry FEC). The receive stage reassembles it and hands `EncodedAudio` to the
  audio thread with `try_send` on a bounded `sync_channel` of `audio.decode_queue_packets`; a full
  queue drops the new packet (`audio_packets_dropped`) and never blocks the receive thread. Audio
  loss is not reported over the control channel and never asks for a keyframe.
- The audio thread opens the decoder itself (Opus, or FLAC with the `flac` feature; FLAC without
  it is `AudioCodecNotCompiled`) on a `CODEC_THREAD_STACK_BYTES` stack, and `connect` waits for
  it. `AudioPipeline` is the testable core: it tracks the next expected frame number, and a gap of
  `n` frames is bridged before the arriving packet is decoded. The first `n - 1` lost frames use
  `conceal` (Opus PLC), the last uses `recover` from the arriving packet (in-band FEC); surround
  and FLAC conceal with silence. At most `audio.max_conceal_ms` worth of frames are produced, so a
  long outage is skipped rather than played back as a stretch of invented audio (`0` turns
  concealment off). A late or duplicate frame number is discarded (`audio_packets_late`), a
  decode error is counted and replaced by one concealed frame, and serial-number arithmetic makes
  wrap transparent. Smoothing against network jitter is the output's own buffer
  (`audio.jitter_buffer_ms` in `AudioPlayback`); the pipeline submits each frame as soon as it is
  decoded.
- `ClientStats` copies the output's `underruns`, `trims` and `overflows` after every packet.
- Cursor (`cursor.rs`): `StreamKind::Cursor` has its own `Reassembler` (a shape image is larger
  than a datagram) and each completed frame is decoded as a `CursorMessage` on the receive
  thread. `LatestCursor`, from `ClientStream::cursor`, holds the latest `CursorState`
  (position, visibility, `Arc<CursorImage>`); a position-only message keeps the previous image's
  `Arc`, so the app re-uploads the shape only when `Arc::ptr_eq` says it changed. `updates` counts
  messages and `wait_after(seen, timeout)` blocks for a newer one. A malformed message counts in
  `malformed_datagrams`; stale frames are dropped by the reassembler. A lost shape message is not
  repaired (see `docs/BUGS.md`).
- DMA-BUF frames from VAAPI fail with `DmaBufPresentUnsupported` until `client-dmabuf-present`.
- `ClientStats` is a snapshot of relaxed atomic counters shared by the stages.
- Unit tests need no hardware: `receive.rs` runs a `ReceiveStage` against a plain loopback
  `Transport` and checks audio and cursor routing, FEC, fragmentation and queue overflow;
  `audio.rs` drives `AudioPipeline` with a scripted decoder and a real Opus round trip.
- Tests (`tests/loopback.rs`, `software` feature) run a test host from `rusplay-net` on loopback
  (`tests/common`): PIN pairing, the `Hello` exchange, the 8-frame AV1 fixture from
  `rusplay-decode/tests/data` cycled at 60 fps through a `Packetizer` with FEC, a drop rule per
  datagram, keyframe restarts on `RequestKeyframe`, Opus audio packets every 5 ms with a drop
  rule, cursor messages and a closing `Goodbye`. They connect with `connect_with_audio` and a
  capturing output, so they need no PipeWire. They return early
  without a GPU adapter. The client config in the tests raises `video.decode_queue_frames`
  because the unoptimised rav1d build is slow.
