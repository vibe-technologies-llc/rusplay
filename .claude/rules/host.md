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
- `serve(&Client, Gpu, &Arc<CaptureFeed>)` returns a `HostStream` at once. The session's
  `ControlChannel` lives in a `ControlOutbox` (`src/outbox.rs`) behind a `parking_lot::Mutex`,
  shared by the control and encode stages; each lock is held for one channel call, never across
  a socket wait. Four to six named threads, each returning `Result<(), HostError>`, share
  `Shared` (stop flag, the streaming flag, `Counters`, the capture mailbox). Any stage that ends
  stops the others; `wait`/`stop` join all of them and return the first error, and dropping the
  stream stops it. `HostError::is_session_failure` tells the
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
  - After every encode it compares `VideoPipeline::stream_format` (codec, the encoder's
    resolution, pixel format and colour) with the last one announced; on a change (the first
    frame, a reopen after a capture resize) it bumps the generation and calls
    `ControlOutbox::announce`, which sends `ControlMessage::StreamFormat` and flushes it on the
    encode thread before the frame is handed to the send stage, so on an ordered path the
    description reaches the client ahead of the keyframe it describes. A full control window
    leaves the announcement pending and the control stage sends it on its next pass; an
    undeliverable announcement that is still the latest is sent again.
  - `rusplay-host-send` owns the video `Packetizer` (created after the keys are installed, FEC
    blocks of up to 32 data shards with `network.fec_overhead_percent` parity) and sends each
    frame with `send_batch`. With `network.pacing` a frame is spread over
    `network.pacing_window_percent` of the frame interval in bursts at least 250 µs apart, so a
    frame never waits behind the previous one's pacing for long.
  - `rusplay-host-control` owns the `ControlChannel` and is the only reader of the socket. It
    answers the client's `Hello` with its own (another version is `VersionMismatch`, after a
    `Goodbye`), then starts the mailbox. `RequestKeyframe`, `BitrateHint` and `ReportLoss` go to
    the mailbox or the counters without blocking; a client `Goodbye` ends the session cleanly.
    The `Hello` answer calls `Shared::start`, which opens the mailbox and sets the streaming flag
    the audio and cursor stages wait for.
    Every `network.keepalive_interval_ms` it sends `Keepalive`, whose acknowledgement is the
    client's proof of life and which keeps the client's own `HostSilent` timer quiet on a static
    screen. No `Hello` within `network.handshake_timeout_ms` is `HandshakeTimeout`; nothing from
    the client for `network.peer_timeout_ms` is `ClientSilent`. On stop it sends `Goodbye` and
    lingers up to four retransmit intervals for its acknowledgement.
  - `rusplay-host-audio` (only when the session has an audio source) opens the encoder from
    `audio.codec` (Opus, or FLAC with the `flac` feature; FLAC without it is
    `AudioCodecNotCompiled`) and the source on its own thread, which runs on a
    `CODEC_THREAD_STACK_BYTES` stack. It reads one codec frame at a time with a 20 ms timeout so
    it sees the stop flag, drops frames until the streaming flag is set (so the first packet is
    number 0 and no stale audio queues up before the client's `Hello`), then encodes, sends each
    packet through its own `Packetizer` on `StreamKind::Audio` (`audio.fec_overhead_percent`,
    0 by default since Opus carries in-band FEC) and counts up by one per packet, which is the
    contract `client.md` describes. It shares nothing with the video path but the stop flag, the
    counters and the socket, so a slow or starved source never delays a video frame; a source
    error ends the session like any other stage.
  - `rusplay-host-cursor` (only when the feed has a cursor subscriber) drains the feed's
    `Subscriber<CursorUpdate>`, merging updates with `CursorUpdate::absorb`, and sends each change
    as a `CursorMessage` on `StreamKind::Cursor` through a `Packetizer` without FEC. Messages are
    latest-wins: updates that arrive while one is being sent merge into the next, so a burst
    collapses to the newest position. The merged state lives on the `CaptureFeed` between
    sessions, so the first message of a session always carries the current image, even when the
    capture reported the shape before the client arrived. A message carries the image when the
    shape changed, when the session's first message goes out, and again every
    `network.cursor_image_repeat_ms` (0 turns repetition off), which repairs a lost image
    message because the client cannot ask for one; the repeat also fires on an idle cursor. A
    bitmap wider or taller than 65535 pixels is `CursorImageTooLarge`. Position, hotspot and
    bitmap stay in capture pixels, never resampled on the host; every message names that space
    (`CursorMessage::space`, the size of the latest captured frame, published by the capture
    stage on the `CaptureFeed` so it survives sessions) and the client scales to the stream. The
    stage sends nothing until a frame has been captured, and a capture resize counts as a
    change, so it sends a message even while the cursor sits still.
- Encode → send is a `sync_channel` of two encoded frames filled with `try_send`. A full queue
  drops the frame without consuming a frame number and forces the next frame to be a keyframe,
  because later frames would reference the dropped one.
- Keyframes: a `RequestKeyframe { frame }` forces the next encode to be a keyframe unless a
  keyframe numbered after `frame` was already sent (`keyframe_requests_ignored`), since the
  client repeats its request until one arrives. Requests coalesce to the earliest frame. With no
  new capture frame pending, the pipeline re-encodes its last encoder input as a keyframe
  (`frames_repeated`), so a static screen still answers.
- `GpuPipeline` (one per session) does capture → GPU → encoder:
  - Shared-memory `XR24`/`AR24`/`XB24`/`AB24` and 10-bit `XR30`/`AR30`/`XB30`/`AB30` frames are
    accepted (`UPLOAD_FORMATS` in `src/upload.rs`): they upload with `write_texture` straight into a
    `Bgra8Unorm`/`Rgba8Unorm` texture (which also has an sRGB view) or an `Rgb10a2Unorm` one, the
    copy the CPU path cannot avoid. `XR30`/`AR30` hold blue in the low bits, so their
    `SourceChannels` say `ChannelOrder::Bgra`. `open_portal_capture` keeps the capture's own
    format order (10-bit first above 8 bits) restricted to those formats, with `prefer_dmabuf`
    off. A DMA-BUF frame is `DmaBufCaptureUnsupported` until `host-dmabuf-capture`; another
    fourcc is `CaptureFormatUnsupported`.
  - Each frame is read by its own `Frame::color`. `Route::plan` (`src/route.rs`, pure and unit
    tested) turns the upload format, the description and the capture size into the stages to run,
    and the pipeline replans only when that `RouteKey` changes, so a renegotiated description
    reconfigures the `Lineariser`s and `ColorMapper` in place (`configure`) from the next frame.
    The mapper (`ColorMapSettings` from `ColorVolume::of_content` of the source and of
    `video.color`, with `color.tone_mapping` and `color.gamut_mapping`) runs unless
    `ColorPlan::is_identity` says it changes nothing.
  - Routes. Encoded: when no mapping is needed, the source transfer is the stream's (and for PQ and
    HLG the `HdrLevels` match too) and there is no linear-light scaling, the encoded image goes
    straight to `ColorConverter` with `InputLight::Encoded`, through a `Lineariser` with
    `OutputLight::Encoded` first only when the channels are not plain RGBA. Gamma-space scaling:
    when `scaling.downscale.linear_light` is off and the stream is smaller, the encoded image is
    scaled, then linearised by a second `Lineariser` at the stream size, then mapped. Linear: an
    sRGB source in a format with an sRGB view is read through that view; anything else is
    linearised by a `Lineariser` at the capture size (PQ and HLG as display light relative to the
    source's reference white); then scale, map and convert with `InputLight::Linear`. Upscaling is
    always in linear light, since the sigmoid needs it.
  - The stream resolution is `video.resolution`, or the capture size when unset. A different size
    runs `Scaler` into an `rgba16float` target (downscale settings, or the upscale ones when either
    axis grows). Scaling runs before mapping. `ColorConverter` (`video.color`,
    `video.pixel_format`, planar, `scaling.downscale.chroma_filter`, left siting, 4-byte rows)
    writes the `YuvFrame`, which is copied into a `MAP_READ` buffer in the same submission.
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
- `AudioSource` (`read_frame(&mut [f32], timeout)`, `overruns`) is the audio capture seam, built
  on the audio thread by an `AudioSourceFactory` (`Arc<dyn Fn(&AudioConfig)>`) so sources need
  not be `Send`. `HostSession::bind` installs `rusplay_audio::AudioCapture` behind the
  `pipewire` feature and no source without it; `with_audio_source` replaces it and
  `without_audio` removes it. Tests that bind a session under `--all-features` must do one of
  the two, or they open the real PipeWire monitor. `FrameSource::cursor_feed` is the matching
  cursor seam (`PipeWireCapture` forwards its `cursor_feed`); `rusplay_capture::latest` builds a
  feed for tests.
- Input injection is not wired: `host-input` in `docs/FEATURES.md`.
- `src/pipeline/tests.rs` runs `GpuPipeline` up to the encoder input (no encoder) and reads luma
  codes back: a 10-bit PQ capture into a PQ stream keeps its codes and channel order, a PQ
  capture into the default SDR stream lands on the BT.709 code of its luminance, a description
  change mid-stream reconfigures and changing back gives identical codes, and a black/white
  stripe downscale gives the linear-light or the gamma-light average as configured. It returns
  early without a GPU adapter.
- Tests: `src/tests.rs` drives the network side without a GPU, over loopback, against a client
  built from `rusplay-net` primitives (its synthetic source produces nothing until the client has
  received the host's `Hello`, after which the mailbox is open), with a `FakePipeline` behind the internal `serve_with`
  (pairing, reconnect, `Hello` gating, keyframe repeat and suppression, bitrate hints, `Goodbye`
  both ways, keepalives, handshake and peer timeouts, the stream format announced before the
  first frame and again with a newer generation after a resize). `src/tests/media.rs` covers the
  audio and cursor stages with synthetic sources (numbering from 0 after the `Hello`, Opus
  decodes, a starved or failing source, cursor image on the first message, burst coalescing,
  image repeat, the image surviving into the next session). `tests/loopback.rs` streams a
  synthetic moving box (described as sRGB, as capture describes an undescribed surface), a tone and a moving cursor through `GpuPipeline` with SVT-AV1 and with
  VAAPI AV1 into the real `rusplay_client::ClientSession` (software decode, a test-only
  dev-dependency, configured for H.264 so only the host's `StreamFormat` can make it decode AV1)
  with a capturing `AudioOutput`, and checks the decoded grey in linear light, the tone's
  amplitude and the final cursor position and image; each returns early without a GPU adapter or
  without the encoder backend and format. The assertions hold on a loaded machine: the client's
  decode queue is raised to its maximum so the warm-up cannot overflow it, and "the first
  presented frame is a keyframe" is written as "or it was superseded", since the latest-wins
  slot may drop the opening keyframe.
