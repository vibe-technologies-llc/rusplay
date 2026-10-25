---
paths:
  - "crates/rusplay-host/**"
---

# Host session

- `HostSession::bind(Config, PeerStore)` binds one `Transport` on `network.bind_address` and
  `network.port` for the life of the host and shares it (`Arc`) with every session; the paired
  clients live in `peers()`, an `Arc<RwLock<PeerStore>>`, so a `forget` applies to the next
  attempt. Sessions run one at a time: `accept`, then `serve`, then `accept` again.
- `accept(Option<Pin>)` drives `PairingResponder` (only when a PIN is given) and
  `ReconnectResponder` side by side on the same socket, by `StreamKind`, until one completes or
  `network.pairing_timeout_ms` passes (`AcceptTimeout`). A spent PIN budget is
  `Net(PairingLocked)`. A PIN pairing is remembered in the store as `client at <ip>` before the
  `Done` goes out. The session keys are installed before the final reply is sent, so the client's
  sealed `Hello` can never arrive first. Instead of `pair_as_host`'s linger, the session keeps
  the client's final handshake datagram (`Confirm` or `Proof`) and its reply (`Replay`), and the
  control stage answers a byte-identical retransmit from it. Any other pairing or reconnect
  datagram during a session is ignored; a restarted client is noticed by the peer timeout.
- `serve(&Client, Gpu, &Arc<CaptureFeed>)` returns a `HostStream` at once. Four named threads,
  each returning `Result<(), HostError>`, share `Shared` (stop flag, `Counters`, the capture
  mailbox). Any stage that ends stops the others; `wait`/`stop` join all four and return the
  first error, and dropping the stream stops it. `HostError::is_session_failure` tells the
  failures that belong to one client (network, handshake, timeouts) from those that will fail
  every session (capture, GPU, encoder).
  - `rusplay-host-capture` locks the `CaptureFeed`'s `FrameSource` for the session and polls
    `next_frame` with a 20 ms timeout so it sees the stop flag. It first re-offers the feed's
    retained frame, so a client joining a static screen still gets a picture.
  - `rusplay-host-encode` builds its `VideoPipeline` on its own thread (the encoder is opened
    there) and waits on the mailbox. The mailbox is latest-wins: a newer frame replaces an
    unconsumed one (`frames_superseded`), and nothing is delivered before the client's `Hello`,
    so the first encoded frame is a keyframe the client will see. Each consumed frame is moved
    into the feed as the retained frame, never copied.
  - `rusplay-host-send` owns the video `Packetizer` (created after the keys are installed, FEC
    blocks of up to 32 data shards with `network.fec_overhead_percent` parity) and sends each
    frame with `send_batch`. With `network.pacing` a frame is spread over
    `network.pacing_window_percent` of the frame interval in bursts at least 250 µs apart, so a
    frame never waits behind the previous one's pacing for long.
  - `rusplay-host-control` owns the `ControlChannel` and is the only reader of the socket. It
    answers the client's `Hello` with its own (another version is `VersionMismatch`, after a
    `Goodbye`), then starts the mailbox. `RequestKeyframe`, `BitrateHint` and `ReportLoss` go to
    the mailbox or the counters without blocking; a client `Goodbye` ends the session cleanly.
    Every `network.keepalive_interval_ms` it sends `Keepalive`, whose acknowledgement is the
    client's proof of life and which keeps the client's own `HostSilent` timer quiet on a static
    screen. No `Hello` within `network.handshake_timeout_ms` is `HandshakeTimeout`; nothing from
    the client for `network.peer_timeout_ms` is `ClientSilent`. On stop it sends `Goodbye` and
    lingers up to four retransmit intervals for its acknowledgement.
- Encode → send is a `sync_channel` of two encoded frames filled with `try_send`. A full queue
  drops the frame without consuming a frame number and forces the next frame to be a keyframe,
  because later frames would reference the dropped one.
- Keyframes: a `RequestKeyframe { frame }` forces the next encode to be a keyframe unless a
  keyframe numbered after `frame` was already sent (`keyframe_requests_ignored`), since the
  client repeats its request until one arrives. Requests coalesce to the earliest frame. With no
  new capture frame pending, the pipeline re-encodes its last encoder input as a keyframe
  (`frames_repeated`), so a static screen still answers.
- `GpuPipeline` (one per session) does capture → GPU → encoder:
  - Only shared-memory `XR24`/`AR24`/`XB24`/`AB24` frames are accepted: they upload with
    `write_texture` straight into a `Bgra8UnormSrgb`/`Rgba8UnormSrgb` texture, whose sRGB view
    linearises on load, which is the copy the CPU path cannot avoid. `open_portal_capture`
    therefore asks PipeWire for exactly those formats with `prefer_dmabuf` off. A DMA-BUF frame
    is `DmaBufCaptureUnsupported` until `host-dmabuf-capture`; another fourcc is
    `CaptureFormatUnsupported`. The captured surface is taken to be sRGB until
    `capture-color-description`.
  - The stream resolution is `video.resolution`, or the capture size when unset. A different size
    runs `Scaler` into an `rgba16float` target (downscale settings, or the upscale ones when either
    axis grows). When `video.color.primaries` is not BT.709 a `ColorMapper` maps sRGB into the
    stream's primaries with `color.gamut_mapping`. `ColorConverter` (linear input,
    `video.color`, `video.pixel_format`, planar, `scaling.downscale.chroma_filter`, left siting,
    4-byte rows) writes the `YuvFrame`, which is copied into a `MAP_READ` buffer in the same
    submission.
  - The readback is mapped, and its planes are repacked row by row into the reused buffer of the
    encoder's input `Frame` in the CPU input contract layout (`EncoderPlanes`: luma pitch of the
    width rounded up to even, chroma pitch half of it when subsampled), which is the one copy out
    of the GPU. One frame is in flight: upload, convert, read back and encode run back to back.
  - The encoder is opened on the first frame and reopened when the stream resolution changes
    (a capture resize with `video.resolution` unset); the new encoder starts with a keyframe and
    keeps the last bitrate hint.
- `FrameSource` (`next_frame(timeout)`) is the capture seam: `PipeWireCapture` implements it
  behind the `pipewire` feature, tests feed synthetic frames. `open_portal_capture` opens the
  portal on the calling thread (`pollster`, restore token as configured) and returns the feed;
  without the `pipewire` feature it is `CaptureNotCompiled`.
- Audio, input injection and the cursor stream are not wired: `host-audio`, `host-input` and
  `host-cursor` in `docs/FEATURES.md`.
- Tests: `src/tests.rs` drives the network side without a GPU, over loopback, against a client
  built from `rusplay-net` primitives, with a `FakePipeline` behind the internal `serve_with`
  (pairing, reconnect, `Hello` gating, keyframe repeat and suppression, bitrate hints, `Goodbye`
  both ways, keepalives, handshake and peer timeouts). `tests/loopback.rs` streams a synthetic
  moving box through `GpuPipeline` with SVT-AV1 and with VAAPI AV1 into the real
  `rusplay_client::ClientSession` (software decode, a test-only dev-dependency) and checks the
  decoded grey in linear light; each returns early without a GPU adapter or without the encoder
  backend and format.
