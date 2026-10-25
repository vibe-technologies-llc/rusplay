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
- Three named threads (`rusplay-client-receive`, `-decode`, `-present`), each returning
  `Result<(), ClientError>`. Any stage that ends sets the shared stop flag; `ClientStream::wait`
  and `stop` join all three and return the first error. Dropping the stream stops it.
- Receive owns the transport, the `ControlChannel` and the video `Reassembler`. It waits in
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
- DMA-BUF frames from VAAPI (`NV12`/`P010`) are imported by a `DmaBufImporter` created on the
  first one, which needs a `Gpu` whose device came from `rusplay_scale::device_descriptor`
  (otherwise the stage ends with `DmaBufImportUnavailable`). The planes are unpacked with
  `RgbConverter::encode_planes` (`SemiPlanar` settings) with no copy, and the decoded frame, the
  surface's lease, goes to `release_after_submission` after the submit, so the decoder reuses the
  surface only once the GPU has read it. CPU frames keep their upload buffer, created on the first
  one. A test (`software` and `vaapi`) presents the AV1 fixtures through both decoders, at the
  source size and upscaled, and requires identical output.
- `ClientStats` is a snapshot of relaxed atomic counters shared by the stages.
- Tests (`tests/loopback.rs`, `software` feature) run a test host from `rusplay-net` on loopback
  (`tests/common`): PIN pairing, the `Hello` exchange, the 8-frame AV1 fixture from
  `rusplay-decode/tests/data` cycled at 60 fps through a `Packetizer` with FEC, a drop rule per
  datagram, keyframe restarts on `RequestKeyframe` and a closing `Goodbye`. They return early
  without a GPU adapter. The client config in the tests raises `video.decode_queue_frames`
  because the unoptimised rav1d build is slow.
