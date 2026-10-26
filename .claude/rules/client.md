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
- Receive → decode is a bounded `sync_channel` of `video.decode_queue_frames` encoded frames,
  filled with `try_send`: encoded frames cannot be dropped silently because later frames reference
  them, so overflow drops the frame and resyncs. Decode → present is a latest-wins `Slot`: a
  decoded frame that is superseded before the GPU took it is dropped (`frames_superseded`), which
  also releases a DMA-BUF lease at once. The decoder is opened on the decode thread (decoders are
  not `Send`) for the codec of the first frame's description and reopened when a description
  names another codec; an open failure ends the stream with `Decode`. `connect` does not wait for
  it (`client-decoder-preopen` in `docs/PERFORMANCE.md`).
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
  matrix and the `HdrLevels` with optional mastering and content light metadata: the host's
  description with whatever the decoder read from the bitstream laid over it,
  `DecodedFrame::color.over(description.color)`, so the bitstream wins), the resolved
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
- Input (`input.rs`): `ClientStream::input()` returns a cloneable `InputHandle` whose `send`
  queues an `InputEvent` for the input stage. The stage, started once the host answered the
  hello (so gamepad `Connected` events are not lost), owns a clone of the `Arc<Transport>` that
  the receive stage also uses (`send_to` takes `&self`), coalesces what has queued up
  (`EventCoalescer`), packs it with `InputPacketer` into `StreamKind::Input` datagrams within
  `Transport::max_datagram_len` and sends them straight away. `InputPacketer` is pure: it splits
  a batch into sections of at most half the datagram budget, repeats the previous section in
  every datagram and emits the empty repeat datagram after state changes
  (`input.resend_delay_ms`). A send error ends the input stage and with it the stream; the gamepad stage never
  returns an error.
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
- Cursor positions and shapes are taken to be in the stream's resolution (`PresentedFrame::source`,
  the description's size); the host currently sends capture pixels (`host-cursor-stream-coordinates`
  in `docs/BUGS.md`). `CursorImage::rgba` is read as sRGB with straight alpha.
- The display's colour target comes from the `display` module (`query_target`,
  `resolve_target`), documented in `color.md`; nothing in the session calls it yet
  (`client-color-pipeline`). `tests/display.rs` has an `#[ignore]`d test against the running
  compositor.
- DMA-BUF frames from VAAPI (`NV12`/`P010`) are imported by a `DmaBufImporter` created on the
  first one, which needs a `Gpu` whose device came from `rusplay_scale::device_descriptor`
  (otherwise the stage ends with `DmaBufImportUnavailable`). The planes are unpacked with
  `RgbConverter::encode_planes` (`SemiPlanar` settings) with no copy, and the decoded frame, the
  surface's lease, goes to `release_after_submission` after the submit, so the decoder reuses the
  surface only once the GPU has read it. CPU frames keep their upload buffer, created on the first
  one. A test (`software` and `vaapi`) presents the AV1 fixtures through both decoders, at the
  source size and upscaled, and requires identical output.
- `ClientStats` is a snapshot of relaxed atomic counters shared by the stages.
- Unit tests need no hardware: `receive.rs` runs a `ReceiveStage` against a plain loopback
  `Transport` and checks audio and cursor routing, FEC, fragmentation, queue overflow and video
  waiting for (and then carrying the newest) stream description;
  `audio.rs` drives `AudioPipeline` with a scripted decoder and a real Opus round trip.
- The loopback host records `StreamKind::Input` datagrams through `InputSequence` and the same
  record decoding a real host uses; the test client config turns `input.gamepads` off so the
  tests never touch `/dev/input`.
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
- Swapchain: `Bgra8Unorm` or `Rgba8Unorm` (`SurfaceFormatUnsupported` otherwise) with its sRGB
  view format, `video.present_mode` (`mailbox` by default, `immediate`, `fifo`; a mode the
  surface lacks is `PresentModeUnsupported`, never a silent fallback) and
  `video.present_frame_latency` (1 by default, 1 to 3), opaque alpha. `OutputLight::Linear`
  frames are drawn through the sRGB view, so the hardware encodes linear light; `Encoded`
  (PQ/HLG) frames go through the plain view with their code values unchanged. That is the
  presentation's whole colour handling: gamut and tone mapping to the display, dithering and
  deeper swapchains belong to `client-color-pipeline`.
- `compose.wgsl` is one fullscreen-triangle render pass that `textureLoad`s the picture 1:1 (no
  sampler, no filtering: sizes always match) and blends the cursor over it as premultiplied
  linear light. The cursor (`window/cursor.rs`) is uploaded once per shape as `rgba16float`,
  linearised from sRGB and premultiplied on the CPU, and when the picture is scaled it is
  resampled with `rusplay-scale`'s `Scaler` (the same upscale or downscale settings as the
  picture, via `present::scale_settings`) into a texture of `round(size * target / source)`,
  rebuilt only when the shape or the scale changes. Its origin is
  `round((position - hotspot) * target / source)`, clipped by the picture.
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
- GPU tests (no compositor needed) compose into an offscreen texture: 1:1 copy, premultiplied
  blend inside the cursor rectangle only, clipping at the edge, and the cursor layer's scaled size
  and origin. The subsurface and swapchain paths need a running compositor and are only checked
  by running the app.
