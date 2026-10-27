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
  `HelloUndeliverable` or `HandshakeAborted`. `probe` only opens the configured decoders.
- Handshake contract the host side must follow: the client sends `ControlMessage::Hello` on the
  control channel right after keys are installed, the host answers with its own `Hello` and starts
  sending video. Before the first video frame, and again whenever any of it changes, the host
  sends `ControlMessage::StreamFormat` with a newer generation; the client decodes and labels
  video only from it, never from its own `video.codec` or `video.color`. A host `Goodbye` ends the stream cleanly; the client sends `Goodbye` on `stop`.
  The host's periodic `Keepalive` only needs the channel's acknowledgement, which every received
  message gets; `rusplay-host` (`.claude/rules/host.md`) is the host side of this contract.
- Three named threads (`rusplay-client-receive`, `-decode`, `-present`), plus `-audio` when an
  audio output exists and, after the handshake, `-input` and, when `input.gamepads` is on,
  `-gamepad`, each returning `Result<(), ClientError>`. Any stage that ends sets the
  shared stop flag; `ClientStream::wait` and `stop` join them all and return the first error.
  Dropping the stream stops it.
- Receive owns the transport, the `ControlChannel` and one `Reassembler` per media stream (video,
  cursor, and audio when there is an output). It waits in
  `Transport::recv_batch_timeout` until the nearest control deadline, reassembly timeout, keyframe
  retry or peer deadline, at most 10 ms, so decode feedback is picked up without a wake-up socket.
  Datagrams from any address but the host are ignored; FEC packets are routed by
  `FecHeader::protects`. Control events of a batch are handled before its video events, so a
  `StreamFormat` that arrived together with the keyframe it describes is applied first.
- The receive stage keeps the newest `StreamDescription` (an older or equal generation is
  ignored), publishes it for `ClientStream::stream_format` and counts it in
  `stream_formats_received`. Every forwarded `EncodedVideo` carries the description in force, and
  its `EncodedFrame::codec` is the description's. A video frame that arrives before any
  description is skipped (`frames_skipped`) and enters resync with a `RequestKeyframe` naming it,
  so nothing waits on the control channel.
- Feedback, all over the control channel, never blocking the frame path: `PacketsMissing` becomes
  `ReportLoss` (dropped and counted when the control window is full, since reports are advisory);
  a `FrameLost` newer than the last keyframe, a gap in completed frame numbers, a frame dropped
  because the decode queue is full, or a decode error on a non-keyframe enters resync: one
  `RequestKeyframe { frame }` naming the first missing frame, repeated every
  `network.keyframe_request_interval_ms` until a keyframe arrives, and every non-keyframe until
  then is skipped (counted in `frames_skipped`). A decode error on a keyframe is fatal. Nothing
  arriving for `network.peer_timeout_ms` ends the stream with `HostSilent`.
- Adaptive bitrate (`bitrate.rs`, on unless `network.adaptive_bitrate` is off): the receive stage
  owns a `BitrateController`, a pure state machine driven by explicit instants and the control
  channel's smoothed RTT, unit tested without I/O. Its target starts at the client's
  `video.bitrate_kbps` (the ceiling). Every congestion signal (`PacketsMissing`, `FrameLost`, a gap
  in completed frame numbers, a decode-queue overflow, a decode failure; not the skip before the
  first `StreamFormat`) cuts it by `network.adaptive_bitrate_backoff_percent`, down to
  `network.adaptive_bitrate_floor_kbps` (lowered to the ceiling if above it), then holds for
  `max(window / 4, 2 × RTT)` so one burst of loss cuts once; each signal also restarts the clean
  window. After a clean window of `max(network.adaptive_bitrate_window_ms, 8 × RTT)` the target
  rises by `network.adaptive_bitrate_step_kbps`, never past the ceiling. Each change becomes a
  `ControlMessage::BitrateHint` through `HintOutbox`: at most one hint in flight, the newest
  pending target sent once the previous is acknowledged (control delivery is unordered, so this is
  what keeps an old hint from overtaking a new one), a full control window leaves it pending, and
  an undeliverable hint is replaced by the current target. Nothing on the frame path waits for it.
  `ClientStats` has `bitrate_hints_sent` and `bitrate_target_kbps` (`None` when disabled).
- Receive → decode is a bounded `sync_channel` of `video.decode_queue_frames` encoded frames,
  filled with `try_send`: encoded frames cannot be dropped silently because later frames reference
  them, so overflow drops the frame and resyncs. Decode → present is a latest-wins `Slot`: a
  decoded frame that is superseded before the GPU took it is dropped (`frames_superseded`), which
  also releases a DMA-BUF lease at once. The decoder is opened on the decode thread (decoders are
  not `Send`). The decode queue carries `DecodeInput`: besides `Video`, the receive stage
  `try_send`s an `Open(codec)` whenever a `StreamFormat` is the first or names another codec, so
  the decoder is set up (rav1d's thread pool, a VAAPI session) before the opening keyframe
  arrives; being in queue order, it never reopens ahead of older frames of the previous codec. A
  full queue drops the `Open` and the first frame opens the decoder as before. The decoder is also
  reopened when a frame's description names another codec; an open failure ends the stream with
  `Decode`, and `connect` does not wait for it.
- Encoded frames carry the host's capture time (`VideoFrameHeader`, stripped by the receive
  stage) as their `Timestamp` and as `captured_micros` through `DecodedVideo` to
  `PresentedFrame::captured`. `PresentedFrame` also has `received_at`, `decoded_at` and
  `submitted_at` instants.
- Latency (`clock.rs`): `ClockSync` (receive-owned, sans-IO) turns `ClockPong`s into a host minus
  client offset (`networking.md`) and publishes it in `HostClock`, which lives in `Counters` so the
  receive, decode and present threads read it. `HostClock::since_capture(captured, at)` maps a
  client `Instant` onto the host stream clock and returns the elapsed time, never negative and
  `None` until the first pong. Each stage records into a smoothed `Gauge` (gain 1/8, single
  writer): `ClientStats::capture_to_receive` at reassembly, `capture_to_decode` when the decoder
  returns the picture, `capture_to_present` when the present stage hands the frame to the sink.
  `clock_offset_micros` is the current offset. The first pong arrives within a round trip of the
  handshake; frames before it carry no figure. The test host's clock is skewed by an hour so the
  loopback test fails if the offset is ignored.
- Present runs on the app's `Gpu` (device and queue): CPU planes are written row by row into the
  `RgbConverter` upload buffer through `Queue::write_buffer_with` (the one unavoidable copy for
  software decode), unpacked to `rgba16float` at the source resolution, taken through the colour
  passes (`present/mapping.rs`, below) and, when the target set by `ClientStream::set_target`
  differs, resampled by `Scaler` (the upscale filter, anti-ringing and, for an SDR colour target
  only, sigmoid when enlarging either axis; the downscale filter otherwise). The passes ping-pong
  between at most two scratch textures at the source resolution and the last one writes the
  output texture, so a frame that needs no colour pass and no scaling is unpacked straight into
  it. Chroma siting is `Left`, matching the host's `ColorConverter` default.
- Output textures are `rgba16float` with `TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC`, pooled up
  to four per target size as `Arc<wgpu::Texture>`; a texture is reused once only the pool holds it,
  so the app may keep a `PresentedFrame` for as long as it displays it. If all four are held the
  frame is skipped (`frames_unpresented`). The app's later submissions on the same queue are
  ordered after the present stage's, so no fence is needed.
- The hand-off is `FrameSink` (`present(&mut self, PresentedFrame)` on the present thread, also
  implemented for `FnMut(PresentedFrame)` closures) and `LatestFrame`, a cloneable latest-wins
  sink with `take` and `wait`. Sinks must not block.
- Every `PresentedFrame` carries `FrameColor`: the `ColorSpace` (primaries, transfer, range,
  matrix and the `HdrLevels` with optional mastering and content light metadata), the resolved
  `MatrixCoefficients`, the coded `PixelFormat` and the `OutputLight` of the texture. The
  source is the host's description with whatever the decoder read from the bitstream laid over
  it (`DecodedFrame::color.over(description.color)`, so the bitstream wins), and it drives the
  unpack: SDR transfers are linearised there, PQ and HLG are unpacked encoded. What the sink
  receives describes the presented texture instead, `FrameColor::mapped_to(&target)`: always
  `OutputLight::Linear`, in the colour target's primaries with 1.0 at its reference white, the
  target's transfer and levels as the encoding still to apply, no mastering or content light
  metadata.
- The colour target is `FrameSink::color_target` (defaulted to `None`; `WindowSink` returns the
  window's, `color.md`), read on every frame, else the `color.target` overrides resolved against
  an empty `DisplayImage` at connect (sRGB at 203 cd/m² by default). `ColorStage` plans a
  `ColorRoute` only when the source colour or the target changes: a `Lineariser` pass for
  encoded (PQ/HLG) frames and a `ColorMapper` pass unless `ColorPlan::is_identity`, fused by default into one
  `ColorChain` that the unpack runs instead of both, or with `color.processing.method = "lut"`
  into one `ColorLut` that the unpack samples (`ColorStage::fused`, the frame unpacked as
  `ColorRoute::unpack` says); the separate passes are left for dynamic peak detection. Its design is in
  `color.md`.
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
- Input (`input.rs`): `ClientStream::input()` returns a cloneable `InputHandle` whose `send`
  queues an `InputEvent` for the input stage. The stage, started once the host answered the
  hello (so gamepad `Connected` events are not lost), owns a clone of the `Arc<Transport>` that
  the receive stage also uses (`send_to` takes `&self`), coalesces what has queued up
  (`EventCoalescer`), packs it with `InputPacketer` into `StreamKind::Input` datagrams within
  `Transport::max_datagram_len` and sends them straight away. `InputPacketer` is pure: it splits
  a batch into sections of at most half the datagram budget, repeats the previous section in
  every datagram and emits the empty repeat datagram after state changes
  (`input.resend_delay_ms`). The stage feeds every event it sends to a `HeldInput` and, every
  `input.snapshot_interval_ms`, packs its `snapshot` records with `InputPacketer::pack_state`
  into datagrams of the same sequence (`input_snapshots_sent`), so the host can repair a key
  whose release was lost with its repeat (`input.md`). A send error ends the input stage and with
  it the stream; the gamepad stage never returns an error.
- The gamepad stage drives `PhysicalGamepads` (5 ms poll, which also bounds how long a rumble
  command or a stop waits), sends its events through the same `InputHandle`, and applies
  `RumbleCommand`s from a channel. The receive stage fills that channel from
  `ControlMessage::Rumble`. Failures are counted (`rumble_failures`, `gamepad_failures`), never
  fatal to the stream. On the way out it sends `Disconnected` for every pad, and the input stage
  waits for it (`gamepads_done`) before its final drain so the host's virtual pads go away.
  `ClientStats` gains `input_packets_sent`, `input_events_sent`, `rumble_commands`,
  `rumble_failures` and `gamepad_failures`.
- `ClientStats` copies the output's `underruns`, `trims` and `overflows` after every packet.
- Cursor (`cursor.rs`): `StreamKind::Cursor` has its own `Reassembler` (a shape image is larger
  than a datagram) and each completed frame is decoded as a `CursorMessage` on the receive
  thread. `LatestCursor`, from `ClientStream::cursor`, holds the latest `CursorState`
  (position, visibility, `Arc<CursorImage>`); a position-only message keeps the previous image's
  `Arc`, so the app re-uploads the shape only when `Arc::ptr_eq` says it changed. `updates` counts
  messages and `wait_after(seen, timeout)` blocks for a newer one. A malformed message counts in
  `malformed_datagrams`; stale frames are dropped by the reassembler. A lost shape message is repaired
  by the host, which repeats the image every `network.cursor_image_repeat_ms` (`host.md`); the
  client never asks for it.
- Every `CursorMessage` carries `space`, the pixel extent its position, hotspot and shape are
  measured in (the host's capture size), kept in `CursorState::space`. The window scales
  position, hotspot and shape by `target / space` per axis, so the cursor lands and is sized
  right whatever the stream resolution is. `CursorImage::rgba` is read as sRGB with straight
  alpha.
- The display's colour target comes from the `display` module (`query_display`,
  `resolve_target`), documented in `color.md`; `StreamWindow::attach` queries it for the first
  output and the output follower keeps it current (`color.md`); the session resolves the
  overrides alone as the fallback. `tests/display.rs` has an `#[ignore]`d test against the
  running compositor.
- DMA-BUF frames from VAAPI (`NV12`/`P010`) are imported by a `DmaBufImporter` created on the
  first one, which needs a `Gpu` whose device came from `rusplay_scale::device_descriptor`
  (otherwise the stage ends with `DmaBufImportUnavailable`). The planes are unpacked with
  `RgbConverter::encode_planes` (`SemiPlanar` settings) with no copy, and the decoded frame, the
  surface's lease, goes to `release_after_submission` after the submit, so the decoder reuses the
  surface only once the GPU has read it. CPU frames (software and NVDEC) keep their upload buffer, created on the first
  one. A test (`software` and `vaapi`) presents the AV1 fixtures through both decoders, at the
  source size and upscaled, and requires identical output.
- `ClientStats` is a snapshot of relaxed atomic counters shared by the stages. `rtt` is the control
  channel's smoothed round trip, republished by the receive stage on every wake.
- Unit tests need no hardware: `receive.rs` runs a `ReceiveStage` against a plain loopback
  `Transport` and checks audio and cursor routing, FEC, fragmentation, queue overflow and video
  waiting for (and then carrying the newest) stream description;
  `audio.rs` drives `AudioPipeline` with a scripted decoder and a real Opus round trip.
- The loopback host records `StreamKind::Input` datagrams through `InputSequence` and the same
  record decoding a real host uses (`InputRecord`, so the periodic held-state snapshot arrives as
  `StateRecord`s that `held_state_snapshots_reach_the_host` looks for); the test client config
  turns `input.gamepads` off so the tests never touch `/dev/input`.
- Tests (`tests/loopback.rs`, `software` feature) run a test host from `rusplay-net` on loopback
  (`tests/common`): PIN pairing, the `Hello` exchange followed by a `StreamFormat`
  (`common::stream_description`: AV1, Display P3, sRGB, full range; the client config names
  H.264, and the fixture's limited range must win over the description's full range), the 8-frame AV1 fixture from
  `rusplay-decode/tests/data` cycled at 60 fps through a `Packetizer` with FEC, a drop rule per
  datagram, keyframe restarts on `RequestKeyframe`, Opus audio packets every 5 ms with a drop
  rule and cursor messages. They connect with `connect_with_audio` and a capturing output, so
  they need no PipeWire, and return early without a GPU adapter.
- The loopback tests are written to hold on a loaded machine, without timeouts stretched to
  hide a race. The test host sends no `Goodbye`; a test waits until the last frame it sent is
  presented (`settle`) and then stops the client, so nothing is cut off mid-decode. The decode
  queue is `u8::MAX` frames, more than any test sends, so warm-up can never overflow it. The
  latest-wins slot may supersede the opening keyframe, so "the first presented frame is a
  keyframe" is asserted as "or `frames_superseded + frames_unpresented` covers the frames before
  it", and the keyframe restart test asserts `frames_skipped`, `decode_errors == 0` and a later
  presented frame rather than that the first one back is a keyframe. Host-side messages are
  awaited with `TestHost::wait_for`, never read immediately. The scaling test holds the host
  back with `HostOptions::gate` until `set_target` has run, so every presented frame is scaled.

## Window (`window` feature)

- `StreamWindow::attach(WaylandTarget, &Config)` (`unsafe`, same pointer contract as
  `WaylandCapture`) presents the stream in an app window. It wraps the window's `wl_display` with
  `Backend::from_foreign_display` and its own event queue (`window/surface.rs`), and makes a child
  `wl_surface` as a desynchronised `wl_subsurface` above the window's surface, with an empty input
  region (pointer and keyboard events keep going to the window's own surface, which is what input
  capture attaches to) and a `wp_viewport`. `wl_compositor`, `wl_subcompositor` and
  `wp_viewporter` are required (`WindowGlobalMissing`).
- It creates its own wgpu instance, a surface on the child `wl_surface`, an adapter compatible
  with it (high performance) and a device from `rusplay_scale::device_descriptor`, so DMA-BUF
  import works. `StreamWindow::gpu` is that device for `ClientSession::connect`, and
  `StreamWindow::sink` the `FrameSink` (`WindowSink`), so frames go from the present stage to the
  window thread without touching the app's UI thread.
- `set_layout(Option<WindowLayout>)` takes the picture rectangle in whole logical pixels of the
  parent surface (`x`, `y`, `width`, `height`) and its device size (`pixels`), as computed by the
  pure `WindowLayout::letterbox(LogicalArea, scale, source)`: the largest rectangle of the source
  aspect inside the area, edges rounded inward to whole logical pixels, centred, device size
  `round(logical * scale)`. The subsurface position is set at once (it applies on the parent's
  next commit); the caller sets `ClientStream::set_target(layout.pixels)` so the present stage
  scales to exactly the swapchain size.
- The `rusplay-client-window` thread waits on one condvar for a new frame, a layout change, a
  cursor update (`LatestCursor` notifies registered `CursorWatcher`s after each message) or stop,
  and keeps the last frame so a cursor move redraws without a new picture. It draws only a frame
  whose texture size equals `layout.pixels`; anything else is a frame scaled for an older layout
  and is skipped. It (re)configures the swapchain to that size, sets the viewport destination to
  the logical size just before presenting (so buffer and destination change in the same commit
  and the compositor maps the buffer 1:1), composes and presents with `Queue::present`.
- Swapchain: format and surface colour space from `SurfaceOutput::choose` (`window/output.rs`,
  `color.md`; `SurfaceOutputUnsupported` when nothing fits the `color.output` choice), no view
  formats, `video.present_mode` (`mailbox` by default, `immediate`, `fifo`; a mode the surface
  lacks is `PresentModeUnsupported`, never a silent fallback) and `video.present_frame_latency`
  (1 by default, 1 to 3), opaque alpha. The choice is made at attach and redone when the output
  the window is on, or its description, changes (`color.md`); the `DisplayEncoding::target` of
  the current choice is what `WindowSink::color_target` and `StreamWindow::color_target` return.
  `stop` stops the output follower after the capture, and returns its error last.
- `compose.wgsl` is one fullscreen-triangle render pass that `textureLoad`s the picture 1:1 (no
  sampler, no filtering: sizes always match), blends the cursor over it as premultiplied
  linear light (converted from BT.709 into the target primaries by a matrix) and encodes and
  dithers the result for the surface (`color.md`). The cursor (`window/cursor.rs`) is uploaded once per shape as `rgba16float`,
  linearised from sRGB and premultiplied on the CPU, and when the picture is scaled it is
  resampled with `rusplay-scale`'s `Scaler` (the same upscale or downscale settings as the
  picture, via `present::scale_settings`) into a texture of `round(size * target / space)`,
  rebuilt only when the shape or the scale changes. Its origin is
  `round((position - hotspot) * target / space)`, clipped by the picture.
- Input: `capture_input(&ClientStream, &InputConfig)` attaches a `WaylandCapture` to the same
  `WaylandTarget` (the window's own surface) with `CaptureOptions::from(input)`, sending into
  `ClientStream::input()`, and sets its region from the current layout; every later `set_layout`
  updates it (`WindowLayout::region`, `None` while there is no layout, which drops absolute
  positions). `is_engaged` and `release` pass through. `is_finished` also reports a capture
  thread that ended, and `stop` stops the capture first (which releases every held key and
  button) and then the presenter, returning the capture's error before the presenter's.
- Surface results: `Timeout`/`Occluded` skip the draw, `Outdated`/`Suboptimal` reconfigure,
  `Lost` and `Validation` end the thread with `SurfaceLost`/`SurfaceValidation`; the app polls
  `is_finished`. `stop` (or drop) joins the thread, which drops the wgpu surface before the
  Wayland objects are destroyed.
- `window/follow.rs` has an `#[ignore]`d test that follows a bound output against the running
  compositor and waits for its description; the follow policy and the stage/switch decisions
  are unit tested in `window/choice.rs` without one.
- GPU tests (no compositor needed) compose into an offscreen texture: 1:1 copy, premultiplied
  blend inside the cursor rectangle only, clipping at the edge, the cursor layer's scaled size
  and origin, and the encodings and dithering listed in `color.md`. The subsurface and swapchain paths need a running compositor and are only checked
  by running the app.

## Host book

- `HostBook` is the client's saved hosts: `SavedHost { name, address, peer, profile }`. It owns
  the `PeerStore` from `PeerStore::open_client` (secrets, keyed by `PeerId`) and a sibling text
  file, the store's path with `.book` added (`paired-hosts.book` by default, so
  `peers.client_file` moves both). The book holds no secret: line one is `rusplay-host-book 2`,
  then one line per host, `<peer id hex or -> <address> <name>`, optionally followed by the
  host's profile as TOML, one tab-prefixed line per TOML line. Version 1 (no profile lines) still
  loads, with empty profiles, and the next `save` writes version 2; a newer version is refused.
  Loading refuses a bad header, address, id or name, duplicate addresses or ids, a profile line
  before any host or in a version 1 book, and a profile that is not valid `HostProfile` TOML
  (unknown keys included) with `HostBookCorrupt` and the line number; an id missing from the
  store loads as unpaired.
- The profile is `rusplay_config::HostProfile`: optional overrides of a fixed subset of client
  options (`decoder`, `present_mode`, `present_frame_latency`, `keyboard`, `pointer`,
  `gamepads`, `pointer_mode`), all absent by default and omitted from the TOML when unset. The
  book stores only the sparse overrides, so unset fields keep following `client.toml`. The app
  merges it at connect with `HostProfile::apply(&Config)` (overlay, then `Config::validate`).
  Adding an overridable option means a field, an `overlay` line and a row in the app's profile
  rows (`ui.md`).
- `save` replaces the whole list: it remembers new or changed pairings in the store, writes the
  book through `rusplay_net::write_owner_only` (the peer store's `0700` directory, `0600` synced
  staging file and rename), then forgets store records no host references. A failed write leaves
  the in-memory list as it was.
