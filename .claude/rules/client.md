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
  `HelloUndeliverable`, `HandshakeAborted` or `NoCommonFormat`. `probe` only opens the configured
  decoders. Before binding, `connect` asks `rusplay_decode::capabilities` for `video.decoder`'s
  formats; the `Hello` carries them, and a host `Hello` whose encodable set shares none of them
  ends the stream with `NoCommonFormat { decodable, encodable }`.
- The client announces its display in the handshake: right after the `Hello`, in the same flush,
  it sends `ControlMessage::Display` (`networking.md`) with its swapchain size, refresh and colour
  volume. `ClientSession::with_display` sets it; without it `announce::default_display` takes
  `video.resolution` (1920x1080 when unset), `video.frame_rate` and the volume
  `color.target` resolves to over the sRGB fallback. `announced_display` builds one from a size, a
  `FrameRate` and a `ColorVolume`, clamping what the protocol would refuse (an axis outside
  `1..=16384`, a refresh outside 1 to 1000 Hz, nits outside 1 to 10000, a black level at or above
  the reference white), so a real display never fails the connect. The app waits until the picture
  canvas has laid out (`StreamView`, below the header and beside the details panel) and sends that
  size as the connect `Display`, with `video.frame_rate` as the refresh and
  `StreamWindow::color_target` as the volume, so a virtual source's first keyframe is already the
  stream area. The display's true refresh is not known and `video.frame_rate` stands in for it. A
  later layout change still goes through `announce_display`. Only a host with a virtual source uses
  it. The client upscale does not run
  when the stream size equals the window's target (`target != source` in the present stage), which is
  the point of sizing the host's output to it.
- `ClientStream::announce_display(ClientDisplay)` stores the newest wanted display in a slot the
  receive stage takes every pass (so it never blocks the caller, and it is cheap to call from a
  layout). `DisplayAnnouncer` (`resize.rs`, pure, unit tested) turns the requests into
  `ControlMessage::Display` sends: a new size is held until it has stayed unchanged for
  `video.display_debounce_ms` (default 200, 0 to 5000, 0 sends at once) and every different size
  restarts the wait, so a window drag sends one message when it stops; a request for the size the
  host already has cancels the pending one; and, because control delivery is unordered, one
  `Display` is in flight at a time (the connect one included) with the newest sent once it is
  acknowledged, an undeliverable one being sent again. A full control window leaves it pending.
  `ClientStats::display_updates_sent` counts the later sends. The app calls it from the canvas
  prepaint with the canvas area in device pixels whenever that changes, so a window resize,
  fullscreen and the header hiding all follow (`ui.md`).
- Handshake contract the host side must follow: the client sends `ControlMessage::Hello` on the
  control channel right after keys are installed, the host answers with its own `Hello` and starts
  sending video. Before the first video frame, and again whenever any of it changes, the host
  sends `ControlMessage::StreamFormat` with a newer generation; the client decodes and labels
  video only from it, never from its own `video.codec` or `video.color`. Its `configured` is the
  host's configured format, so `ClientStream::stream_format()` and `is_fallback` show a
  negotiated downgrade instead of hiding it. A host `Goodbye` ends the stream cleanly; the client sends `Goodbye` on `stop`.
  The host's periodic `Keepalive` only needs the channel's acknowledgement, which every received
  message gets; `rusplay-host` (`.claude/rules/host.md`) is the host side of this contract.
- Three named threads (`rusplay-client-receive`, `-decode`, `-present`), plus `-audio` when an
  audio output exists and, after the handshake, `-microphone` when `audio.microphone.enabled`,
  `-input` and, when `input.gamepads` is on,
  `-gamepad`, each returning `Result<(), ClientError>`. Any stage that ends sets the
  shared stop flag; `ClientStream::wait` and `stop` join them all and return the first error.
  Dropping the stream stops it.
- The receive, decode and present threads apply `rusplay_priority::ThreadTuning`
  (`.claude/rules/priority.md`) as the first thing they do, in `ClientStream::spawn`: receive as
  `ThreadRole::TimerDriven` (it waits on timeouts), decode and present as `ThreadRole::Frame`, and
  the audio and microphone threads as `ThreadRole::TimerDriven` as well, so a decode thread busy
  with a software decoder or a VAAPI readback cannot keep them at the default nice and starve the
  playback ring. The input and gamepad threads keep the default. A refused request leaves the
  thread at its default and is counted in `ClientStats::thread_priority_failures`.
- Receive owns the transport, the `ControlChannel` and one `Reassembler` per media stream (video,
  cursor, and audio when there is an output). It waits in
  `Transport::recv_batch_timeout` until the nearest control deadline, reassembly timeout, keyframe
  retry or peer deadline, at most 10 ms, so decode feedback is picked up without a wake-up socket.
  Datagrams from any address but the host are ignored; FEC packets are routed by
  `FecHeader::protects`. Control events of a batch are handled before its video events, so a
  `StreamFormat` that arrived together with the keyframe it describes is applied first.
- Clipboard: `ClientSession::with_clipboard` / `without_clipboard` and the `window` feature's
  default data-control opener, exactly like the host's (`clipboard.md`); the app passes its own
  opener with the focus fallback. `connect_stream` opens a
  `ClipboardLink` when `input.clipboard` is not `off` and hands it to the receive stage in
  `Links::clipboard`; the receive stage passes `ControlMessage::Clipboard` chunks to it once the
  handshake is done and calls `service` once per loop pass, never on the frame path.
  `ClientStream::clipboard()` is the `ClipboardState`. A protocol error counts in
  `malformed_datagrams`, a backend error drops the link.
- The receive stage keeps the newest `StreamDescription` (an older or equal generation is
  ignored), publishes it for `ClientStream::stream_format` and counts it in
  `stream_formats_received`. Every forwarded `EncodedVideo` carries the description in force, and
  its `EncodedFrame::codec` is the description's. A video frame that arrives before any
  description is skipped (`frames_skipped`) and enters resync with a `RequestKeyframe` naming it,
  so nothing waits on the control channel.
- Feedback, all over the control channel, never blocking the frame path: `PacketsMissing` becomes
  `ReportLoss` (dropped and counted when the control window is full, since reports are advisory);
  a `FrameLost` newer than the last keyframe, a gap in completed frame numbers, a frame dropped
  because the decode queue is full enters resync. The first request of that resync is
  `RequestRecovery { frame, last_good }` when `network.loss_recovery = "reference"` (the default)
  and the decoder has a last good frame, else `RequestKeyframe { frame }`, naming the first missing
  frame. A decode error is not that path. FFmpeg 9 frees every reference it has already output
  while building the failed list, so a recovery frame would name a picture the decoder no longer
  holds (`Could not find ref with POC`). `DecodeFailed` asks for a keyframe at once
  (`keyframe_only`) and both the receive and decode stages drop every later frame until that
  keyframe arrives. A recovery frame the decoder still rejects, on the loss path, is not requested
  again: the next request is a keyframe. While the decoder accepts pictures, a later loss may try
  recovery once more. An unanswered request is repeated every
  `network.keyframe_request_interval_ms` as `RequestKeyframe` until a keyframe or a usable
  recovery frame arrives, and every other frame until then is skipped (counted in
  `frames_skipped`). A recovery frame is usable when its reference is at or before the last good
  frame and not before the last keyframe (`Resync::can_resume_from`, `reference_recoveries`). A
  decode error on a keyframe is fatal.
- `Gpu::guard` uses a mutex shared by every clone of the client GPU to serialize GPU work
  and the device's error scopes across the processing and window threads. A window resize must
  keep the queue idle throughout `Surface::configure`; concurrent processing submissions can
  otherwise fail configuration with `GpuWaitTimeout`. Configuration has its own checked error
  scope, and the presenter marks the surface configured only after that scope succeeds, before
  acquiring a texture. The ignored Wayland test
  `resizing_while_frames_are_submitted_keeps_the_surface_configured` exercises 200 size changes
  while another thread submits GPU work; it reproduces the configure failure without the mutex.
- The last good frame comes from the decode stage: every frame its decoder accepts is sent back
  as `Feedback::Decoded`, and the receive stage keeps the newest (`Resync::last_good`, cleared when
  a new codec reopens the decoder). It is acknowledged to the host as `AcknowledgeFrame` at most
  every `network.frame_ack_interval_ms` (default 50, 0 sends none) and only when it changed
  (`frame_acks_sent`). The decode stage, skipping after its own decode error, resumes only on a
  keyframe. A loss that never reached the decoder still resumes on a recovery frame whose reference
  is at or before the last frame it decoded. Nothing
  arriving for `network.peer_timeout_ms` ends the stream with `HostSilent`.
- `frames_lost` counts each frame once: a `FrameLost` and a completed frame both advance
  `last_frame` (`Resync::advance_to`), and only the frame numbers that never produced a packet
  between `last_frame` and the new frame are added on top, so a frame FEC could not recover is
  not counted again by the gap check on the next completed frame.
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
  The decode loop waits on the queue with a 50 ms `recv_timeout` and checks the stop flag on each
  wake, so a stage that fails while `connect` still holds the queue's sender (the audio decoder
  failing to open) stops the decode thread, which closes the slot and ends present, instead of
  leaving `connect` joined on stages that never end.
- Encoded frames carry the host's capture time (`VideoFrameHeader`, skipped by the receive
  stage through `FrameBytes::skipping`, so the reassembled frame is not moved) as their `Timestamp` and as `captured_micros` through `DecodedVideo` to
  `PresentedFrame::captured`. `PresentedFrame` also has `received_at`, `decoded_at` and
  `submitted_at` instants, and `captured_at`, the capture mapped onto the client's `Instant`
  clock through `HostClock` at submission (`None` before the first pong); a re-scaled retained
  frame keeps its original `captured_at`.
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
- `Gpu` carries the device's `GpuFaults` (`Gpu::new` installs the uncaptured-error handler; tests that
  share a device use `Gpu::with_faults`). The present stage runs each frame and retarget, and the
  window thread each draw, inside `Gpu::guard`, so a wgpu validation, out-of-memory or internal error
  becomes `ClientError::Gpu(GpuFault)` instead of wgpu's panic, and decoded, target and window sizes
  past `max_texture_dimension_2d` are refused up front with `GpuFault::TextureTooLarge`.
- Present runs on the app's `Gpu` (device and queue): CPU planes are written row by row into the
  `RgbConverter` upload buffer through `Queue::write_buffer_with` (the one unavoidable copy for
  software decode), unpacked to `rgba16float` at the source resolution, taken through the colour
  passes (`present/mapping.rs`, below) and, when the target set by `ClientStream::set_target`
  differs, resampled by `Scaler` (the upscale filter, anti-ringing and, for an SDR colour target
  only, sigmoid when enlarging either axis; the downscale filter otherwise). The passes ping-pong
  between at most two scratch textures at the source resolution and the last one writes the
  output texture, so a frame that needs no colour pass and no scaling is unpacked straight into
  it. Each pooled output texture and scratch texture keeps its view beside it (`Image`,
  `Retained::image`), so the stages' bind group caches hit every frame. Chroma siting is the one the decoder read from the bitstream (`SignalledColor::chroma_siting`), then the host's `StreamDescription::siting` when the bitstream signals none (`decode::resolve_siting`, which AV1 needs for centred), and `scaling.chroma_siting` (default `Left`) only when `DecodedVideo::siting` is `None`, which a stream from a Rusplay host never is.
- The present stage keeps the last frame's pre-scale image (`Retained`: the scratch texture the
  scaler read, or the output texture itself when the frame was not scaled, no copy) with the
  frame's metadata. `ClientStream::set_target` with a changed target nudges the decoded `Slot`
  (`Slot::take_or_nudge`, a value still comes first), and the stage then re-scales the retained
  image into a pooled output at the new size, or copies it when the target equals the source, and
  hands it to the sink as the same frame (`frame`, `captured` and the instants unchanged,
  `submitted_at` new). It is not counted in `frames_presented`, so a static remote screen
  follows a resize, fullscreen or the header hiding without new video.
- Output textures are `rgba16float` with `TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC | COPY_DST`, pooled up
  to six per target size as `Arc<wgpu::Texture>` (created only when every existing one is held:
  the window's drawn frame, its two paced frames, the pending one and the retained one); a texture is reused once only the pool holds it,
  so the app may keep a `PresentedFrame` for as long as it displays it. If all six are held the
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
  it. `AudioPipeline` is a thin wrapper that publishes the tallies of
  `rusplay_audio::ConcealingPlayer` as counters. The player tracks the next expected frame number, and a gap of
  `n` frames is bridged before the arriving packet is decoded. The first `n - 1` lost frames use
  `conceal` (Opus PLC), the last uses `recover` from the arriving packet (in-band FEC); surround
  and FLAC conceal with silence. At most `audio.max_conceal_ms` worth of frames are produced, so a
  long outage is skipped rather than played back as a stretch of invented audio (`0` turns
  concealment off). A late or duplicate frame number is discarded (`audio_packets_late`), a
  decode error is counted and replaced by one concealed frame, and serial-number arithmetic makes
  wrap transparent. Smoothing against network jitter is the output's own buffer
  (`audio.jitter_buffer_ms` in `AudioPlayback`); the pipeline submits each frame as soon as it is
  decoded.
- Microphone (`microphone.rs`): when `audio.microphone.enabled`, `connect` opens an `AudioInput`
  once the host answered the hello, so the capture ring holds no stale audio when sending
  starts. The default (`default_microphone`) is `AudioCapture` on `CaptureTarget::Source` behind
  the `pipewire` feature and `MicrophoneNotCompiled` without it; `connect_with_devices` takes
  any `Option<Box<dyn AudioInput>>` (`connect_with_audio` passes none). The stage opens an Opus
  encoder for `AudioConfig::microphone_stream` on a `CODEC_THREAD_STACK_BYTES` stack, and
  `connect` waits for it. It reads one frame at a time with a 20 ms timeout so it sees the stop
  flag, encodes, and sends the packet through a `Packetizer` on `StreamKind::Microphone` with
  `send_to`, numbered from 0 counting up by one. Nothing queues between capture and socket: a
  slow stage lets the capture ring drop its newest frames (`microphone_overruns`), and a starved
  source sends nothing and consumes no number. `MicrophoneEncoder` is the testable core. A
  source or send error ends the stage and with it the stream.
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
  `PadFeedback`s from a channel. The receive stage fills that channel from
  `ControlMessage::Rumble`, `Lightbar`, `PlayerLeds` and `TriggerEffect`. Rumble counts in
  `rumble_commands` and `rumble_failures`; any other feedback counts in `feedback_commands` only
  when `PhysicalGamepads::apply` applied it, in `feedback_dropped` when it had nowhere to go (no
  such LED, hidraw node or pad), and in `feedback_failures` when it failed; `gamepad_failures`
  counts the stage's own poll failure. None is
  fatal to the stream. On the way out it sends `Disconnected` for every pad, and the input stage
  waits for it (`gamepads_done`) before its final drain so the host's virtual pads go away.
  `ClientStats` gains `input_packets_sent`, `input_events_sent`, `rumble_commands`,
  `rumble_failures`, `feedback_commands`, `feedback_dropped`, `feedback_failures` and `gamepad_failures`.
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
- `stats_json.rs` turns a `ClientStats` into one JSON line, `stats_json_line(phase, unix_ms, &stats)`,
  in the same shape as the host's (`{"event":"client_stats","phase":"interval"|"final","unix_ms":..,`
  then every counter under its field name). The microsecond values are suffixed `_us` (`rtt_us`,
  `clock_offset_us`, `capture_to_receive_us`, `capture_to_decode_us`, `capture_to_present_us`) and
  are `null` until measured, as is `bitrate_target_kbps`. A test compares the emitted keys with the
  `Debug` output of `ClientStats`, so a new counter that is not added fails it. The library never
  prints: the app emits the lines (`ui.md`).
- `ClientStats` is a snapshot of relaxed atomic counters shared by the stages. `rtt` is the control
  channel's smoothed round trip, republished by the receive stage on every wake.
- Unit tests need no hardware: `receive.rs` runs a `ReceiveStage` against a plain loopback
  `Transport` and checks audio and cursor routing, FEC, fragmentation, queue overflow and video
  waiting for (and then carrying the newest) stream description;
  `audio.rs` drives `AudioPipeline` with a scripted decoder and a real Opus round trip;
  `microphone.rs` encodes a scripted source and decodes the datagrams back.
- The loopback host records `StreamKind::Input` datagrams through `InputSequence` and the same
  record decoding a real host uses (`InputRecord`, so the periodic held-state snapshot arrives as
  `StateRecord`s that `held_state_snapshots_reach_the_host` looks for); the test client config
  turns `input.gamepads` off so the tests never touch `/dev/input`.
- Tests (`tests/loopback.rs`, `software` feature) run a test host from `rusplay-net` on loopback
  (`tests/common`): PIN pairing, the `Hello` exchange (`HostOptions::encodable` is the set the
  test host answers with; one whose set shares nothing with the client's decodable formats, empty when libavcodec H.264 and HEVC are compiled in, checks `NoCommonFormat`) followed by a `StreamFormat`
  (`common::stream_description`: AV1, Display P3, sRGB, full range; the client config names
  H.264, and the fixture's limited range must win over the description's full range), the 8-frame AV1 fixture from
  `rusplay-decode/tests/data` cycled at 60 fps through a `Packetizer` with FEC, a drop rule per
  datagram, keyframe restarts on `RequestKeyframe` and `RequestRecovery`, Opus audio packets every 5 ms with a drop
  rule and cursor messages. The test host also records `StreamKind::Microphone` datagrams, which
  the microphone tests reassemble. They connect with `connect_with_audio` (or
  `connect_with_devices` for a scripted microphone) and a capturing output, so they need no
  PipeWire, and skip visibly (`src/hardware.rs`; a failure under `RUSPLAY_REQUIRE_HW=1`) without a GPU adapter.
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
  cursor update (`LatestCursor` notifies registered `CursorWatcher`s after each message), stop,
  or the due time of the earliest paced frame (`wait_until`, the one deliberate wait on the frame
  path), and keeps the last drawn frame so a cursor move redraws without a new picture. A frame
  arriving only wakes it (`Shared::arrive`, no `dirty`): it is scheduled and drawn when due, while
  a layout, cursor, overlay or display change redraws the current frame at once. It draws only a frame
  whose texture size equals `layout.pixels`; anything else is a frame scaled for an older layout
  and is skipped; a `None` layout unmaps the subsurface (a null buffer attach and commit) and the
  next drawn frame maps it again. It (re)configures the swapchain to that size, sets the viewport destination to
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
  `round((position - hotspot) * target / space)` (the subtraction saturates), clipped by the
  picture. A shape whose image or scaled size exceeds the device's `max_texture_dimension_2d` is
  not drawn and counts once per shape and size in `StreamWindow::cursors_skipped`.
- Statistics overlay (`window/overlay.rs`): `StreamWindow::set_overlay(Option<OverlayText>)` sets up
  to `MAX_OVERLAY_LINES` lines of at most `MAX_OVERLAY_COLUMNS` characters, drawn in the picture's
  top-left corner. The subsurface sits above gpui's surface, so the overlay is part of
  `compose.wgsl` and not a separate surface. `OverlayLayer` rasterizes the text on the CPU from a
  built-in 5x7 bitmap font (`window/overlay/font.rs`: space, digits, letters shown in capitals,
  and `.,:-/%+()=_`; anything else is `?`) into an `r8unorm` ink mask scaled by
  `target.height / 540` (at least 1), and re-uploads it only when the text or the scale changes.
  The shader blends it after the cursor in linear target light: the rectangle darkens the picture
  by a constant plate alpha and the ink is 1.0, the target's reference white, and the result is
  encoded and dithered with everything else. `None`, blank text or a mask past
  `max_texture_dimension_2d` draws nothing. The overlay is stored in `Pending` like the layout,
  so setting it wakes the window thread and a static picture redraws with the retained frame.
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
- Presentation feedback (`window/surface.rs`, `window/presentation.rs`): `ChildSurface` binds
  `wp_presentation` (versions 1 and 2) on its queue when the compositor offers it; it is optional
  (`PresentationStats::feedback` says whether it exists). Right after `Queue::submit` and before
  `Queue::present`, the window thread sends `wp_presentation.feedback` for the child surface
  with a `Submission` (submit instant, `captured_at` for the first draw of a frame only, the
  targeted vblank). The feedback request is double-buffered surface state applied by the next
  `wl_surface.commit`, and the swapchain's present is what commits: Mesa's Vulkan WSI calls
  `wl_surface_commit` inside `vkQueuePresentKHR` on the same `wl_display`, so the request is
  marshalled ahead of that commit in the one libwayland buffer, the same ordering
  `set_destination` already relies on. The only other commit on the surface is `unmap`, which
  never follows a feedback request. Events are read by whoever reads the display (gpui's loop or
  the output follower) and dispatched by the window thread on every wake (`ChildSurface::dispatch`,
  then `take_feedback`, at most 64 outcomes kept). `presented` timestamps come in the clock the
  compositor names in `clock_id` (realtime, monotonic, monotonic-raw or boottime are read with
  `clock_gettime`; another clock drops the sample) and are mapped onto `Instant` by the distance
  from that clock's now (`pacing::instant_of`). `StreamWindow::presentation()` returns
  `PresentationStats`: presented, discarded and coalesced frame counts, smoothed submit-to-display
  (`display_latency`) and capture-to-display latency, the last `refresh` (`None` for a variable or
  unknown rate, which the protocol reports as 0), the current pacing margin and the last
  `PresentationFlags` (`vsync`, `hw_clock`, `hw_completion`, `zero_copy`).
- Pacing (`window/pacing.rs`, pure and unit tested with synthetic instants): `video.present_pacing`
  is `off` (default: draw on arrival, the previous behaviour), `latency` or `smooth`, with
  `video.present_margin_us` (3 ms) and `video.present_jitter_us` (8 ms). `VblankModel` takes its
  phase from the last presented feedback with the `vsync` flag and its period from `refresh`; a
  zero refresh marks the display variable, and a phase older than 2 s is unknown. `latency`
  submits one margin before the first vblank that is still reachable (`next vblank at or after
  arrival + margin`), so frames arriving before that deadline coalesce into one present; without
  a known fixed refresh it submits on arrival. `smooth` keeps the minimum capture-to-arrival delay
  over the last 120 frames and aims each frame at `captured_at + minimum + jitter` (never before
  arrival, so late frames go at once): on a variable display it submits at that time, on a fixed
  one it targets the vblank nearest it (not earlier than half a refresh before) and submits one
  margin ahead; without `captured_at` it submits on arrival. The margin adapts: a frame shown more
  than half a refresh after its targeted vblank widens it by a quarter refresh (at most one
  refresh), every hit relaxes it by 1/16 of the excess back to the configured value.
  `PacedQueue` holds at most two scheduled frames: a new one replaces any queued frame due no
  earlier, a full queue drops its oldest, and of several frames due at once only the newest is
  drawn; all three count in `frames_coalesced`.
- What the stack does not expose, and so is not used: wgpu offers only the present modes (no
  present timing, no target time, no VRR control); VRR is the compositor's and the display's
  business, and a variable rate is only seen as `refresh = 0` in the feedback, on which pacing
  presents at host cadence. `wp_commit_timing_v1` and `wp_fifo_v1` (staging, in the locked
  wayland-protocols 0.32.13) cannot be used by Rusplay: a surface may carry only one
  `wp_commit_timer_v1` and one `wp_fifo_v1` (a second is the `commit_timer_exists` /
  `fifo_manager_already_exists` protocol error, fatal to the shared gpui connection), and Mesa's
  Wayland WSI (Mesa 25 and later; the 26.2 radv driver references `wp_commit_timer_v1` and
  `wp_fifo_v1`) creates its own on the swapchain's surface when the compositor offers them, for
  its FIFO and present-timing paths, which wgpu does not drive with target times. Target
  presentation times are therefore scheduled with the CPU timer.
- `window/follow.rs` has an `#[ignore]`d test that follows a bound output against the running
  compositor and waits for its description; the follow policy and the stage/switch decisions
  are unit tested in `window/choice.rs` without one. `window/surface.rs` has an `#[ignore]`d test
  that requests feedback for a commit of a role-less surface, destroys it and expects exactly one
  outcome through `SurfaceEvents` with a readable presentation clock; it also prints whether the
  compositor advertises `wp_commit_timing_manager_v1` and `wp_fifo_manager_v1`.
- GPU tests (no compositor needed) compose into an offscreen texture: 1:1 copy, premultiplied
  blend inside the cursor rectangle only, clipping at the edge, the cursor layer's scaled size
  and origin, the overlay's pixels inside its rectangle only and nothing drawn without one, and the encodings and dithering listed in `color.md`. The subsurface and swapchain paths need a running compositor and are only checked
  by running the app.

## Host book

- `HostBook` is the client's saved hosts: `SavedHost { name, address, host_id, peer, profile }`. It owns
  the `PeerStore` from `PeerStore::open_client` (secrets, keyed by `PeerId`) and a sibling text
  file, the store's path with `.book` added (`paired-hosts.book` by default, so
  `peers.client_file` moves both). The book holds no secret: line one is `rusplay-host-book 3`,
  then one line per host, `<peer id hex or -> <host id hex or -> <address> <name>`, optionally followed by the
  host's profile as TOML, one tab-prefixed line per TOML line. `host_id` is the
  `HostId` the host announces in discovery (absent for a host never seen announcing, or one that
  announces none; the all-zero id is stored as absent). Version 1 (no profile lines) and version 2
  (no host id field) still load, with empty profiles and no host ids, and the next `save` writes
  version 3; a newer version is refused.
  Loading refuses a bad header, address, peer id, host id or name, duplicate addresses, peer ids or host ids, a profile line
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
