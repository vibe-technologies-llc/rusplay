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
  `Net(PairingLocked)`. A PIN pairing is remembered in the store under the name the client announced in
  `Start` (`client at <ip>` when it announced none) before the `Done` goes out. The session keys are installed before the final reply is sent, so the client's
  sealed `Hello` can never arrive first. Instead of `pair_as_host`'s linger, the session keeps
  the client's final handshake datagram (`Confirm` or `Proof`) and its reply (`Replay`), and the
  control stage answers a byte-identical retransmit from it. A pairing
  datagram from the client during a session is ignored, and a `Start` or a `Hello` for another
  peer from any other address is answered `Rejected(Busy)` (`networking.md`).
  - Takeover: a reconnect `Hello` for the session's own peer, from any address, runs through
    the control stage's `SessionTakeover` (the peer store is refreshed first, like `accept`; a refresh that fails refuses the takeover with
    `Rejected(Busy)` and leaves the session as it is, never falling back to the old list).
    Only on the authenticated `Accepted` the stage installs the new session keys, answers, and
    restarts the session for the new client: `Shared::suspend` (streaming flag off, mailbox held
    and a keyframe forced), `ClientLink::switch` (the shared client address every stage reads,
    plus an epoch), `ControlOutbox::reset` (a fresh `ControlChannel`), the `Replay` replaced by
    the takeover's `Proof` and `Accepted`, pending control events dropped, and the handshake timer
    and peer timer restarted. The new client then says `Hello` like a fresh one: the control stage
    resends the latest `StreamFormat` before `Shared::start`, so the description precedes the forced
    keyframe. When the epoch changes the input stage lifts everything the old client held
    (`release_all`), starts a fresh `HeldInput` and `InputSequence`, and the cursor stage resends
    the image. `HostStats::sessions_taken_over` counts them. The old client is simply
    dropped: its datagrams no longer unseal. The microphone stage, which numbers nothing itself but
    follows the client's numbering, starts a fresh `Reassembler` and calls
    `ConcealingPlayer::restart` when the epoch changes, since a new client numbers packets from 0.
- `serve(&Client, Gpu, &Arc<CaptureFeed>)` returns a `HostStream` at once. The session's
  `ControlChannel` lives in a `ControlOutbox` (`src/outbox.rs`) behind a `parking_lot::Mutex`,
  shared by the control and encode stages; each lock is held for one channel call, never across
  a socket wait. Four to six named threads, each returning `Result<(), HostError>`, share
  `Shared` (stop flag, the streaming flag, `Counters`, the capture mailbox). Any stage that ends
  stops the others, and so does a panic (every thread body runs inside a `StopOnDrop` guard, so
  an unwinding build never leaves control sending keepalives); `wait`/`stop` join all of them and return the first error, and dropping the
  stream stops it. `HostError::is_session_failure` tells the
  failures that belong to one client (network, handshake, timeouts) from those that will fail
  every session (capture, GPU, encoder).
  - `HostSession::busy_flag()` is an `Arc<AtomicBool>` that every `HostStream` raises when it
    is created and lowers when it is dropped (`BusyGuard`), for the discovery responder
    (`rusplay-hostd`) to announce; accepting and pairing alone do not raise it.
  - The encode, send and control threads apply `ThreadTuning` (`src/priority.rs`) as the first
    thing they do: `network.frame_thread_nice` (default -5, 0 leaves the thread alone) through
    `setpriority`, which Linux applies to the calling thread, and, on the send thread only,
    `network.send_timer_slack_us` (default 1, 0 leaves the kernel's) through
    `PR_SET_TIMERSLACK`, so the pacer's short sleeps do not overshoot. A refused request (no
    `RLIMIT_NICE` headroom) is silent and the thread runs at its default.
  - `rusplay-host-capture` locks the `CaptureFeed`'s `FrameSource` for the session and polls
    `next_frame` with a 20 ms timeout so it sees the stop flag. It stamps each frame with the
    session's `StreamClock` the moment `next_frame` returns (`Captured`), and that stamp travels
    with the frame through the mailbox and `Outgoing` to the send stage; a keyframe repeat is
    stamped when it is produced. It first re-offers the feed's
    retained frame, so a client joining a static screen still gets a picture.
  - `rusplay-host-encode` runs the `PipelineFactory` on its own thread: `encodable` first
    (`GpuPipelineFactory` runs `rusplay_encode::capabilities` for `video.encoder` and builds the
    `ScalePipelines` while the handshake is still in flight) and offers the set on
    `Shared::negotiation`, then waits for the control stage's `NegotiatedFormat` and `build`s the
    `VideoPipeline` for the chosen codec and pixel format (the encoder is opened there, lazily on
    the first frame), and waits on the mailbox. The mailbox is latest-wins: a newer frame replaces an
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
  - `rusplay-host-send` passes each encoded frame's `VideoFrameHeader` to the packetizer as a
    prefix (`packetize_prefixed`, one reused 8-byte buffer, the frame is not copied) and owns the video `Packetizer` (created after the keys are installed, FEC
    blocks of up to 32 data shards with `network.fec_overhead_percent` parity) and sends each
    frame with `send_batch`. With `network.pacing` a frame is spread over
    `network.pacing_window_percent` of the frame interval, the shorter of the configured
    `video.frame_rate` interval and the smoothed spacing of the frames' capture stamps (so a
    faster capture never overlaps its own pacing), in bursts at least 250 µs apart, so a
    frame never waits behind the previous one's pacing for long. With `network.pacing_max_mbps`
    above 0 the frame is instead sent at that rate when it fits inside the window (a frame
    that would take longer at the cap still takes the whole window, so the window stays the
    bound); 0 keeps the window-only behaviour and is the default until loss has been measured
    on real links.
  - `rusplay-host-control` owns the `ControlChannel` and is the only reader of the socket. It
    keeps the client's `Hello` (another version is `VersionMismatch`, after a `Goodbye`) until the
    encode stage has offered its encodable set, then answers with its own `Hello` carrying that
    set and negotiates (`src/negotiate.rs`, the order in `networking.md`): the agreed
    `NegotiatedFormat` goes to `Shared::negotiation` before the mailbox starts, and
    `HostStream::format` exposes it (`rusplay-hostd` logs it once, naming both formats on a
    fallback). No shared format answers the `Hello`, says `Goodbye` and ends the session with
    `NoCommonFormat`, a session failure. After a session takeover the format is never
    renegotiated, since the encoder is already built for it: the new client's `Hello` must list the
    agreed format (`resume_after_takeover`), otherwise the session ends with `NoCommonFormat`.
    `RequestKeyframe`, `BitrateHint` and `ReportLoss` go to
    the mailbox or the counters without blocking; a `ClockPing` is answered at once with a
    `ClockPong` carrying the stream clock; a client `Goodbye` ends the session cleanly.
    A `BitrateHint` goes through `HintPolicy` (`HintPolicy::from_config`): it is clamped to
    `network.bitrate_hint_min_kbps..=network.bitrate_hint_max_kbps` (`bitrate_hints_clamped`), or
    ignored (`bitrate_hints_ignored`) when the encoder runs `RateControl::ConstantQuality`, which
    has no bitrate to steer. `HostStats::bitrate_target_kbps` is the bitrate in force:
    `video.bitrate_kbps` at session start, then the last applied hint; `None` under constant
    quality.
    The `Hello` answer calls `Shared::start`, which opens the mailbox and sets the streaming flag
    the audio and cursor stages wait for. `StreamKind::Microphone` datagrams, and `Fec` datagrams
    that protect it, are forwarded after `Hello` with `try_send` (tagged with the `ClientLink` epoch,
    and the stage drops one from an earlier epoch) to the microphone stage's bounded
    channel (`audio.decode_queue_packets` datagrams; a full channel counts
    `microphone_packets_dropped`), so the control stage stays the only socket reader.
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
  - `rusplay-host-microphone` (only when `audio.microphone.enabled`) runs on a
    `CODEC_THREAD_STACK_BYTES` stack. It opens an Opus decoder and the output from
    `AudioConfig::microphone_stream`, reassembles the forwarded datagrams (`Reassembler` for
    `StreamKind::Microphone`, `network.reassembly_timeout_ms`) and feeds each packet to a
    `rusplay_audio::ConcealingPlayer` whose output is the virtual `Audio/Source` node (see
    `audio.md`). Loss, late packets, concealment, decode errors, underruns and overflows are the
    `microphone_*` counters. An output error ends the session like any other stage. Enabled
    with no output (no `pipewire` feature, or `without_microphone`) fails the session with
    `MicrophoneNotCompiled`; disabled, the stage never starts and the datagrams are ignored.
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
- Encode → send is a `sync_channel` of one encoded frame filled with `try_send` (a deeper queue
  only adds latency behind a frame still being paced). A full queue
  drops the frame without consuming a frame number and forces the next frame to be a keyframe,
  because later frames would reference the dropped one.
- Keyframes: a `RequestKeyframe { frame }` forces the next encode to be a keyframe unless a
  keyframe numbered after `frame` was already sent (`keyframe_requests_ignored`), since the
  client repeats its request until one arrives. Requests coalesce to the earliest frame. With no
  new capture frame pending, the pipeline re-encodes its last encoder input as a keyframe
  (`frames_repeated`), so a static screen still answers. The forced flag is cleared only once a frame reached the send queue, so a
  pipeline that produced nothing (an import that fell back to shared memory, a repeat with no
  input yet) or a full queue leaves the next frame a keyframe.
- `GpuPipeline` (one per session) does capture → GPU → encoder:
  - DMA-BUF frames are the default path and are never copied: `GpuPipeline` imports them with its
    own `DmaBufImporter` (created on the first DMA-BUF frame, its cache keeping a capture
    swapchain's imports) and binds the imported view, or its sRGB `linear_view`, as the first
    stage's source. The route's `UploadFormat` comes from the import (texture format, sRGB view if
    the modifier kept one, `DmaBufTexture::channels`), so swizzled formats such as `BX24` go
    through the `Lineariser` like any other channel order. `Gpu::open` creates the device with
    `rusplay_scale::device_descriptor`, which is what makes import possible.
  - wgpu's default error handler panics, which `panic = "abort"` makes a dead daemon, so `Gpu::open`
    installs `rusplay_scale::GpuFaults` as the device's uncaptured-error handler and
    `GpuPipeline::encode` runs each frame inside `Gpu::guard`: error scopes for validation,
    out-of-memory and internal errors, then a check of anything the handler recorded. Each comes out as
    `HostError::Gpu(GpuFault)`, which ends the session like any stage error; `repeat` checks the
    recorded faults too. A capture or stream resolution beyond the device's
    `max_texture_dimension_2d` is refused up front with `GpuFault::TextureTooLarge`. A `Gpu` built
    some other way needs its own `GpuFaults::install`.
  - `feed_from_session(session, config, gpu)`, behind both `open_portal_capture(config, gpu)` and
    the combined input session of `open_portal`, keeps the capture's own format order (10-bit
    first above 8 bits) restricted to `UPLOAD_FORMATS`, and offers for each one the modifiers
    `DmaBufImporter::modifiers` says the device imports (`capture_options`, unit tested), never
    the implicit one. With no importer or no importable modifier at all, `prefer_dmabuf` is turned
    off and the capture asks for shared memory.
  - A DMA-BUF frame that fails to import (importer unavailable, unsupported format or modifier)
    makes the pipeline call the feed's `ShmFallback::request` (`FrameSource::shm_fallback`, which
    `PipeWireCapture` forwards) and skip the frame, so PipeWire renegotiates to shared memory and
    the session keeps going; a source without a fallback returns the import error. Only a
    rejection of the buffer itself (`rejects_dmabuf`: importer unavailable, unsupported format or
    modifier, plane layout or extent) asks for it, once per stream (`dmabuf_fallbacks`), since
    the capture keeps `prefer_dmabuf` off for good. Any other import error (importer creation,
    memory import, fence syscalls) is taken as transient: the frame is dropped
    (`dmabuf_import_failures`) and the next one retries DMA-BUF, until 60 failures in a row end the
    session with the error. The encode stage reads these counts from `VideoPipeline::take_notices`
    after every encode and `rusplay-hostd` prints them with the other stats.
  - Before the GPU reads a DMA-BUF the pipeline waits for its write fences (`gate_on_writes`).
    A buffer whose fences have already signalled (a zero-timeout `wait_for_writes`) is read
    straight away. Otherwise, with `capture.dmabuf_gpu_fence_wait` on (default) and a
    `GpuFenceWaiter` (the host's device from `Gpu::open` carries `VK_KHR_external_semaphore_fd`
    where the driver has it), the fence is staged as a queue wait semaphore, held in
    `Source::fence` and released right after `convert` submits, so the host thread never blocks
    on the producer. Without a waiter, or when staging fails (a kernel without
    `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`), it falls back to `rusplay_scale::wait_for_writes` for up to
    `capture.dmabuf_fence_timeout_ms` (default 100, 0 only checks); a timeout reads the buffer
    as it is rather than dropping the frame, so on this path a producer whose fences never signal
    still streams. A staged GPU wait cannot be abandoned that way, so the device poll that follows
    the submission (hand-off wait or readback) is bounded by `capture.dmabuf_fence_timeout_ms` plus
    a fixed 500 ms allowance for the GPU work; a producer whose fence never signals ends the
    session with `GpuFenceTimeout` (never retried as a hand-off failure). Polls with no staged fence
    are not bounded.
  - The frame, and so its PipeWire buffer lease, is held until the readback or hand-off wait has
    seen the GPU finish, and then as the feed's retained frame; the encode stage drops the previous retained
    frame before encoding the next, so a session holds at most three capture buffers.
  - Shared-memory `XR24`/`AR24`/`XB24`/`AB24` and 10-bit `XR30`/`AR30`/`XB30`/`AB30` frames are
    accepted (`UPLOAD_FORMATS` in `src/upload.rs`): they upload with `write_texture` straight into a
    `Bgra8Unorm`/`Rgba8Unorm` texture (which also has an sRGB view) or an `Rgb10a2Unorm` one, the
    copy the CPU path cannot avoid. `XR30`/`AR30` hold blue in the low bits, so their
    `SourceChannels` say `ChannelOrder::Bgra`. Another fourcc is `CaptureFormatUnsupported`.
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
    always in linear light, since the sigmoid needs it. The sigmoid is left off when the source transfer is PQ or HLG, whose linear
    light runs above reference white and would be clamped by the sigmoid's 0..1 input.
  - The stream resolution is `video.resolution`, or the capture size when unset. A different size
    runs `Scaler` into an `rgba16float` target (downscale settings, or the upscale ones when either
    axis grows). Scaling runs before mapping. `ColorConverter` (`video.color`,
    `video.pixel_format`, `scaling.downscale.chroma_filter`, `scaling.chroma_siting`) writes the `YuvFrame`
    into one of two targets (`Target` in `src/pipeline.rs`): exported memory handed to the encoder
    on the GPU, or a buffer read back to shared memory.
  - GPU hand-off (`src/handoff.rs`): with `video.encoder_gpu_handoff` on (default),
    `Handoff::plan` takes `rusplay_encode::device::device_input` for the backend and format (VAAPI
    4:2:0 as a DMA-BUF in `NV12`/`P010`/`P012`; NVENC 4:2:0 `NV12`/`P010`, 4:2:2 `NV16`/`P210` and 4:4:4 `YU24`/`Q410`
    as an opaque fd), and `GpuPipeline::new` keeps it only when `rusplay_scale::can_export` says the
    device exports that handle type. The converter then writes semi-planar, or `PlanarHigh` (MSB
    samples) for 4:4:4, with 256-byte rows (`HANDOFF_ROW_ALIGNMENT`) straight into memory from
    `ColorConverter::create_exported_frame`, once `check_device_layout` has accepted the planes
    (`Handoff::accepts`; a unit test runs every hardware format at several sizes through it).
    After the submit the encode thread waits for that submission alone (`PollType::Wait` with its
    index) and hands the encoder a `Frame` holding a dup of the exported fd: `FrameBuffer::DmaBuf`
    with the linear modifier and a detached lease, or `FrameBuffer::OpaqueFd` with the
    allocation's id, size and dedicated flag. That wait is the only CPU stall and it is needed:
    neither VAAPI nor NVENC's CUDA-pointer input takes a fence, and RADV allocates exported memory
    explicitly synchronised, so implicit sync on the dma-buf would not hold the VCN back. One frame
    is in flight: the encoder returns only after reading its input, so the next conversion reuses
    the same buffer and `repeat` re-encodes it. The exported buffer survives a converter
    reconfiguration that keeps the layout; VAAPI imports it once (its cache keys by inode), NVENC
    keys it by the allocation id.
  - Fallback: the readback path runs for software encoders, for formats without a device input
    (VAAPI 4:2:2/4:4:4 would need the packed `YUYV`/`AYUV` family, which the converter does not
    write), with `video.encoder_gpu_handoff` off, and when the device cannot export. The hand-off
    starts as `HandoffState::Trying`: an export, layout or encode failure before the first
    handed-off frame has encoded turns it `Off` for the pipeline, and that same frame is converted
    again into the readback target and encoded from shared memory by a freshly reopened encoder (the
    failed one may hold state from the discarded frame), so a driver that refuses the import costs
    one extra conversion and one encoder reopen; `HostStats::handoff_disabled` counts it. Once a frame has encoded (`Proven`), errors end the session
    like any other encoder error.
  - Readback: the converter writes planar with 4-byte rows, the frame is copied into a `MAP_READ`
    buffer in the same submission, mapped, and its planes are repacked row by row into the reused
    buffer of the encoder's input `Frame` in the CPU input contract layout (`EncoderPlanes`: luma
    pitch of the width rounded up to even, chroma pitch half of it when subsampled), which is the
    one copy out of the GPU. One frame is in flight: upload, convert, read back and encode run
    back to back.
  - Measured at 3840x2160 on an RX 9070 XT (radeonsi, VCN 5) from a DMA-BUF `XR24` capture,
    `GpuPipeline::encode` from import to encoded frame, median of 240 frames with both paths run
    round robin (`cargo test --release -p rusplay-host --all-features --lib handoff_frame_times --
    --ignored --nocapture`): 8-bit AV1 and HEVC take 5.4 ms handed off against 9.6 ms read back,
    10-bit (`P010`) 10.6 ms against 25 to 27 ms, where the readback also pays the CPU shift into
    `P010`. GPU clocks swing a whole run by up to 2x (one run gave 8-bit AV1 10.9 against 17.1 ms),
    so only paths timed in the same run compare.
  - The encoder is opened on the first frame and reopened when the stream resolution changes
    (a capture resize with `video.resolution` unset); the new encoder starts with a keyframe and
    keeps the last bitrate hint.
- `FrameSource` (`next_frame(timeout)`) is the capture seam: `PipeWireCapture` implements it
  behind the `pipewire` feature, tests feed synthetic frames. `open_portal_capture(config, gpu)`
  opens the portal on the calling thread (`pollster`, restore token as configured) and returns the
  feed;
  without the `pipewire` feature it is `CaptureNotCompiled`.
- `AudioSource` (`read_frame(&mut [f32], timeout)`, `overruns`) is the audio capture seam, built
  on the audio thread by an `AudioSourceFactory` (`Arc<dyn Fn(&AudioConfig)>`) so sources need
  not be `Send`. `HostSession::bind` installs `rusplay_audio::AudioCapture` behind the
  `pipewire` feature and no source without it; `with_audio_source` replaces it and
  `without_audio` removes it. Tests that bind a session under `--all-features` must do one of
  the two, or they open the real PipeWire monitor. `FrameSource::cursor_feed` is the matching
  cursor seam (`PipeWireCapture` forwards its `cursor_feed`); `rusplay_capture::latest` builds a
  feed for tests.
- `MicrophoneOutputFactory` (`Arc<dyn Fn(&AudioConfig) -> Result<Box<dyn AudioOutput>>>`, called
  on the microphone thread with the derived mono config) is the microphone seam.
  `HostSession::bind` installs the virtual-source `AudioPlayback` behind the `pipewire` feature
  and nothing without it; `with_microphone_output` replaces it and `without_microphone` removes
  it. Tests that enable the microphone must replace it or they publish a real PipeWire node.
- Input: `HostSession::with_input(Arc<InputFeed>)` turns it on; without a feed the host drops
  `StreamKind::Input` datagrams. `InputFeed` holds an `InputTarget` (`inject(InputEvent)` and
  `inject_batch`, which defaults to `inject` per event, and `service(&mut Vec<RumbleCommand>)`)
  behind a mutex that the session's input stage holds for the
  session, so tests inject a recording target and no portal is involved. `InputFeed::new` takes a
  `Send` target; `InputFeed::spawn` builds a target that cannot leave its thread (`LibeiInjector`
  holds non-`Send` callbacks) on the dedicated `rusplay-host-injector` thread and drives it through
  a request channel (each call waits at most `INJECT_TIMEOUT`, 250 ms, then fails with
  `SinkClosed`, and a late reply is told apart by its request id), servicing it every 50 ms while no session uses it so the compositor's pings
  are answered between clients. The injector thread ends when the feed drops.
  - `rusplay-host-input` (only with a feed) receives the datagrams the control stage forwards
    once the `Hello` is answered (earlier ones are dropped) over a bounded `sync_channel` of
    `INPUT_QUEUE_DATAGRAMS` (256) filled with `try_send` (a full channel counts
    `input_packets_dropped`, so a stalled compositor never queues input to replay late as a
    burst). Each datagram carries the `ClientLink` epoch it was forwarded under and the stage drops
    one from an earlier epoch (`input_packets_stale`) so a takeover never lets the old client's
    datagrams raise the new sequence. It reads them with `InputPacket::read`
    and `InputSequence::admit` (a fresh sequence per session), decodes each record with
    `InputRecord::decode` and injects the events of a section as one `inject_batch` (a single round
    trip to the injector thread, one result per event, each recorded in `HeldInput` only on
    success); a state record first flushes the events before it, then is turned into the events
    that bring the host to the client's held state (`HeldInput::reconcile`,
    `input_events_repaired`), which are injected as a batch too. A failed injection
    is not recorded as held, so the next snapshot tries again. A malformed datagram or record
    counts in `malformed_datagrams`, a stale one in `input_packets_stale`, and a failed injection
    in `input_failures` without ending the session, since a paused device or a closed compositor
    seat must not take the video down. Every 4 ms it calls `InputTarget::service` and sends each
    `RumbleCommand` as a `ControlMessage::Rumble` through the `ControlOutbox`; a full control
    window drops the command (`rumble_dropped`), because the client caps every effect length. On
    the way out it releases whatever `HeldInput` says the client still held.
    The portal's `InputTarget::service` always polls both halves, the libei connection and the
    virtual pads' rumble, and returns the first error, so a compositor that dropped EIS never leaves
    a pad's force-feedback upload undrained.
  - `open_portal(&Config, &Gpu)` returns a `PortalHost { feed, input }`; the GPU is needed for the
    capture's importable modifiers, so the GPU is opened before the portal. With `input.accept_remote` it
    opens the combined RemoteDesktop + ScreenCast session (`input.md`) and an `InputFeed` over
    `SplitSink<LibeiInjector, UinputGamepads>`; a build without the `input` feature (or without
    `pipewire`) fails with `InputNotCompiled` (`CaptureNotCompiled`), and
    `input.accept_remote = false` opens the plain ScreenCast session with no feed.
    `open_portal_capture` stays for callers that want capture only.
- `src/pipeline/tests.rs` runs `GpuPipeline` up to the encoder input (no encoder) and reads luma
  codes back: a 10-bit PQ capture into a PQ stream keeps its codes and channel order, a PQ
  capture into the default SDR stream lands on the BT.709 code of its luminance, a description
  change mid-stream reconfigures and changing back gives identical codes, and a black/white
  stripe downscale gives the linear-light or the gamma-light average as configured. DMA-BUF tests
  build udmabuf frames: imported `XR24` and `XR30` give the same codes as the same shared-memory
  frames, `XB24` and `BX24` read their channel order, and an unimportable frame requests the
  shared-memory fallback (or fails without one). It returns early without a GPU adapter, and the
  DMA-BUF tests also without `/dev/udmabuf` or the import feature. `src/pipeline/tests/handoff.rs`
  encodes a moving pattern with VAAPI AV1 (8 and 10-bit, at 640x360 and 402x226) once through the
  hand-off and once through the readback, decodes both with rav1d and requires identical samples
  (the hand-off must also have stayed on), checks that a failing encoder turns the hand-off off,
  and holds the ignored 4K timing test; each returns early without the backend and format.
- Tests: `src/tests.rs` drives the network side without a GPU, over loopback, against a client
  built from `rusplay-net` primitives (its synthetic source produces nothing until the client has
  received the host's `Hello`, after which the mailbox is open), with a `FakePipeline` behind the internal `serve_with`
  (pairing, reconnect, session takeover with its wrong-proof and `Busy` cases, `Hello` gating, format negotiation with fallback and with no overlap, keyframe repeat and suppression, bitrate hints, `Goodbye`
  both ways, keepalives, handshake and peer timeouts, the stream format announced before the
  first frame and again with a newer generation after a resize). `src/tests/input.rs` drives the
  input stage over loopback with a recording `InputTarget` (delivery in order, nothing before the
  `Hello`, a snapshot repairing a stuck key, rumble back as control messages, held input released
  when the session ends, failing injections counted, no feed ignores input) and `src/input.rs`
  unit-tests the datagram handling
  and the injector thread. `src/tests/media.rs` covers the
  audio and cursor stages with synthetic sources (numbering from 0 after the `Hello`, Opus
  decodes, a starved or failing source, cursor image on the first message, burst coalescing,
  image repeat, the image surviving into the next session). `tests/loopback.rs` streams a
  synthetic moving box (described as sRGB, as capture describes an undescribed surface), a tone and a moving cursor through `GpuPipeline` with SVT-AV1 (readback) and with
  VAAPI AV1 (GPU hand-off) into the real `rusplay_client::ClientSession` (software decode, a test-only
  dev-dependency, configured for H.264 so only the host's `StreamFormat` can make it decode AV1)
  with a capturing `AudioOutput`, and checks the decoded grey in linear light, the tone's
  amplitude and the final cursor position and image. Two more cover negotiation end to end:
  SVT-AV1 configured for 4:4:4 falls back to 4:2:0 and the client's description shows it, and
  an x265 host against the AV1-only software client fails with `NoCommonFormat` on both ends. Each returns early without a GPU adapter or
  without the encoder backend and format. The assertions hold on a loaded machine: the client's
  decode queue is raised to its maximum so the warm-up cannot overflow it, and "the first
  presented frame is a keyframe" is written as "or it was superseded", since the latest-wins
  slot may drop the opening keyframe. The same holds for `src/tests.rs` on a starved machine: the
  send thread can fall behind and the full queue drops a frame, which forces a keyframe by design,
  so a keyframe after the first is only asserted to be covered by a counted drop. The loopback
  test also sends a key through the client's `InputHandle` (with a short
  `input.snapshot_interval_ms`) and checks it reaches the host's recording target, that snapshots
  arrive, and that the host releases the key when the stream ends.
