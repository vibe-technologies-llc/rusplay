---
paths:
  - "crates/rusplay-capture/**"
---

# Capture

- Capture goes through the xdg-desktop-portal ScreenCast interface (`ashpd` with the `async-io`
  runtime, zbus's own `default-rt`, rather than tokio, and the `screencast` feature)
  and then the PipeWire node the portal returns. No compositor-specific protocols
  (wlr-screencopy, ext-image-copy-capture) until the portal path is complete, and no X11.
- `capture.source = "virtual"` (`SourceKind::Virtual`) asks the portal for `SourceType::VIRTUAL`,
  a monitor the compositor creates for the stream. The default stays `monitor`. Before
  `select_sources` the session reads the portal's `AvailableSourceTypes` and a portal without the
  virtual bit is `CaptureError::SourceUnavailable { requested }` (`PortalSession` and
  `SharedScreencast` both check; `check_source_available` is the pure part, unit tested). The
  mapping of every kind is `portal_sources`, tested too. A virtual stream's size is compositor
  specific: Mutter sizes it from the negotiated format, KWin chooses itself, so the size a frame
  really has (`NegotiatedFormat::size`, then `Frame::resolution`) is the truth and nothing here
  assumes the offered size was taken.
- What KWin 6.8.2 does with the virtual source, measured in a nested headless `kwin_wayland
  --virtual` on its own D-Bus with the portal frontend and backend, a client in the same process
  and the shared PipeWire daemon (`crates/rusplay-hostd/tests/nested/`, `hostd.md`). It is the
  virtual backend, loopback and no display: nothing here is a statement about a real output, a
  real GPU scanout or a second machine, and Mutter has not been run
  (`capture-virtual-output-live` in `docs/FEATURES.md`).
  - `AvailableSourceTypes` is 7 (monitor, window and virtual), so the virtual bit is there.
  - The portal still shows its `Screen Sharing Requested` dialog for a virtual source;
    xdg-desktop-portal-kde does not skip it. Once the restore token is saved a restart skips it.
  - KWin creates the output at 1920x1080. The first frame has that size (and, with no hold
    on the first encode, so does the first keyframe); 80 ms to 0.5 s after the offer the stream is exactly the offered size (1280x720,
    1000x700 and 2560x1440 tried). The host holds its first encode until a frame of the offered
    size arrives (`virtual_first_frame_timeout_ms`, `host.md`), so the client's first keyframe has
    the offered size and there is a single keyframe; with the hold off the encoder sends the stale
    keyframe first and a second one at the offered size. A 120x90 offer KWin ignores (below) is
    streamed at 1920x1080 once the timeout passes, its first frame reaching the client 1.5 s after
    connecting against 0.4 to 1.0 s when the offered size arrives. KWin's `EnumFormat` takes 200x200 to 10000x10000 with
    the current size as the default, so a display under 200 pixels on an axis cannot be offered.
    An offer of 120x90 sent past the host's clamp (`capture.virtual_min_resolution` set to 1x1) was
    taken by nothing: the stream stayed at the size it had (1280x720), kept delivering frames and
    raised no error, so the failure is silent and the host must never send such an offer
    (`select_output` raises it to 267x200, which KWin took).
  - Live resizes: three further offers on a running stream (1000x700, 1600x900, 800x600) each
    reached the client as exactly that size, one keyframe per size; a flood of 120 window sizes
    10 ms apart became one `Display` from the client (its settle time) and one offer, and the
    stream ended on the last size, 1376x838. A hostile client skipping that debounce is bounded by
    the host's own gate (`host.md`), which the nested KWin runs do not exercise.
  - The offered rate is not honoured: the format carries a variable `framerate` (0/1) and a
    `maxFramerate` of the output's refresh, 60 Hz. Offers of 30, 90 and 144 Hz all captured 60
    frames a second from a damaged output (`capture-virtual-refresh-kwin`).
  - A virtual output nothing draws on delivers one frame after the renegotiation, at the new
    size, and then nothing until something is drawn on it.
  - The stream has no colour description: KWin's `EnumFormat` lists no `transferFunction` or
    `colorPrimaries`, so every frame is the sRGB/BT.709 fallback of `describe_color`. The portal
    has no request that takes the client's colour volume, so it cannot reach the output through
    it; KWin's own output management (HDR, ICC) was not tried.
- `OutputOffer` (a cloneable handle from `PipeWireCapture::output_offer`, usable without the
  `pipewire` feature) takes an `OutputRequest { resolution, frame_rate }` after the stream is
  already running: the request goes into a shared slot and a `Command::Offer` wakes the PipeWire
  thread, which rebuilds the `EnumFormat`s with `StreamOptions::offering` (the size and rate become
  the defaults of the `size` and `framerate` ranges, never a fixed value, so a compositor that picks
  its own size still negotiates) and calls `update_params`, the same renegotiation the shared-memory
  fallback uses. The worker also adopts the slot before every fixation and fallback pod, so a later
  renegotiation never offers the stale size. A failed update is stored as the stream's failure and
  ends the loop. Only the host's virtual source uses it (`host.md`).
- The portal session asks for persistence (`PersistMode::ExplicitlyRevoked`) when
  `capture.persist_session` is on. `PortalSession::open_persistent` reads the restore token from
  a `RestoreTokenStore` (an explicit `capture.restore_token` wins), and writes the token the portal
  returns after a successful start, so the next run skips the picker. A stale token just brings the
  picker back. The file is `capture.restore_token_file`, defaulting to
  `$XDG_STATE_HOME/rusplay/restore-token` (ignored unless absolute, per the XDG spec) or
  `~/.local/state/rusplay/restore-token`; it is written to a staging file with mode 0600 and
  renamed into place. With persistence off the store is neither read nor written.
- `SharedScreencast` joins a ScreenCast to a session another portal owns (any
  `IsScreencastSession`, today the RemoteDesktop session of `rusplay-input`): `select_sources`
  sends the cursor mode and source types but not persistence, which a RemoteDesktop session takes
  at `SelectDevices`, and `open_remote` turns the streams that session's `Start` returned into a
  `PortalSession` for `PipeWireCapture::from_portal`.
- PipeWire consumption lives behind the `pipewire` feature (`src/pipewire/`). `PipeWireCapture`
  connects to the portal's remote fd and node id and implements `CaptureSource`. The PipeWire main
  loop runs on its own `rusplay-pipewire` thread; setup errors come back synchronously from
  `connect`, later ones (stream error state, a malformed buffer) are stored and returned by the
  next `next_frame` once the handoff closes.
- Frames cross threads through `handoff::latest`, a single-slot mailbox: a new frame replaces an
  unconsumed one (counted by `superseded_frames`), so the consumer always gets the newest frame
  and nothing queues. `process` also drains PipeWire's queue down to the newest buffer. `latest`,
  `Publisher` and `Subscriber` are public so other crates can build a feed (the host's tests make
  cursor feeds with them), which also keeps them used without the `pipewire` feature.
- The format pods, the buffer → `Frame` conversion and the SPA ↔ DRM fourcc table are pure
  functions in `format.rs` and `convert.rs`, unit tested without a daemon. SPA names formats in
  byte order and DRM in little-endian word order, so SPA `BGRx` is DRM `XR24`. Formats are
  offered in `StreamOptions::formats` order; 10-bit (`XR30` family) goes first unless
  `video.pixel_format.depth` is pinned to 8. The negotiated `framerate` (or `maxFramerate` for a
  variable stream) is published through `PipeWireCapture::source_refresh` (`auto.md`).
- The surface's colour description is read from the negotiated `Format` pod's
  `SPA_FORMAT_VIDEO_colorRange`, `colorMatrix`, `transferFunction` and `colorPrimaries` ids
  (`parse_format` into `SpaColorimetry`, 0 meaning unknown). `describe_color` in
  `pipewire/color.rs` is the pure function from `SpaColorimetry` and the configured `HdrLevels`
  (`StreamOptions::hdr`, from `video.color.hdr`) to the `ColorSpace` every `Frame` carries
  (`Frame::color`, set from the negotiated format, so a renegotiation changes it from the next
  frame). Per field: unknown transfer is sRGB, unknown primaries are BT.709, or BT.2020 when the
  transfer is PQ or HLG; SPA BT.709/BT.601/SMPTE 240M/BT.2020 transfers are the BT.709 curve,
  gamma 1.0 is linear, gamma 2.2 and Adobe RGB are gamma 2.2; range 16..235 is limited, anything
  else full; the matrix is set only for BT.709, BT.601 and BT.2020 (RGB, FCC and SMPTE 240M leave
  it unset, since the captured formats are all RGB). BT.470M, generic film, Adobe RGB and EBU 3213
  primaries become `ColorPrimaries::Custom` from the `Chromaticities` constants in core. A value
  with no model (other gamma curves, log, SMPTE ST 428) fails negotiation with
  `TransferUnsupported` or `PrimariesUnsupported` rather than being guessed. PipeWire has no
  mastering display or content light level metadata, so those stay unset (`capture-hdr-metadata`
  in `docs/FEATURES.md`).
- Every `EnumFormat` offers `transferFunction` and `colorPrimaries` as `Id` enum choices
  (`ColorOffer` from `StreamOptions::color_preference`, which `StreamOptions::new` sets to
  `video.color` when `video.color_mode` is `configured`; with `source` there is no offer and the
  producer keeps its own description): the stream's own transfer and primaries first (BT.2020 for custom primaries
  under PQ/HLG, BT.709 otherwise), then every other id `describe_color` can model
  (`DESCRIBABLE_TRANSFERS`, `DESCRIBABLE_PRIMARIES`), so an HDR-capable compositor picks the
  stream's encoding and a producer's fixed choice is never refused. The properties carry
  `SPA_POD_PROP_FLAG_DROP` (`PROP_FLAG_DROP`, defined locally because older headers lack it): a
  producer that does not describe its colour drops them in the intersection instead of having
  our default copied into the negotiated format, which would label an sRGB surface as the
  stream's transfer. The flag exists from PipeWire 1.5.81, so on an older library
  (`pw_get_library_version`, `drops_unmatched_properties`) the offer is left out and the producer's
  description passes through unconstrained. An unfixated colour choice in a negotiated format
  reads as its default. A 10-bit `xBGR_210LE` surface is HDR only when the producer says so.
- Shared-memory frames are copied out of the mapped buffer (`MAP_BUFFERS`) as `stride × height`
  bytes from the chunk offset, and the buffer is requeued at once. The copy lands in a `ShmBytes`
  (`rusplay-core`) taken from the worker's `ShmPool` (`src/pool.rs`), which keeps at most three
  idle `Vec`s and gets each one back when the frame is dropped, so a 4K frame is a `memcpy`
  into warm pages, not a fresh allocation and its page faults, and a superseded frame costs no
  allocation either. The PipeWire buffer is not leased for the frame's life as a DMA-BUF's is: the
  mapping belongs to libpipewire and is unmapped when the stream goes, which a frame can outlive,
  so a borrowed view would dangle (a dup of the memfd mapped per frame would pay the same page
  faults as the allocation). Buffers whose chunk is empty or
  flagged corrupted (cursor-only updates) produce no frame. Frame timestamps are microseconds
  since the stream started, taken on arrival.
- Frames are DMA-BUF whenever the compositor offers them (`capture.prefer_dmabuf`), so they reach
  the GPU scaler and hardware encoder without a CPU copy. Shared memory is the fallback, not the
  default.
- DMA-BUF negotiation: for each format with acceptable modifiers, an `EnumFormat` carries
  `SPA_FORMAT_VIDEO_modifier` as a `Long` enum choice flagged `MANDATORY | DONT_FIXATE`; the
  shared-memory `EnumFormat` (no modifier) follows them as the fallback. When the negotiated
  `Format` still holds a modifier choice, the consumer fixates it with the first modifier of its own
  list that the producer offers and calls `update_params` with that fixated pod followed by all the
  original ones, so a producer that fixates on its own still converges. A choice with no acceptable
  modifier renegotiates with shared memory only. A fixed modifier requests `SPA_DATA_DmaBuf`
  buffers, no modifier requests `MemPtr | MemFd`. `decide` in `format.rs` is the pure decision.
- The acceptable modifiers are injected per format through `StreamOptions::modifiers`
  (`FormatModifiers`, also usable without the `pipewire` feature), so the GPU side can supply the
  modifiers it can import (`rusplay_scale::DmaBufImporter::modifiers`). The default is
  `DEFAULT_MODIFIERS`: `DRM_FORMAT_MOD_INVALID` (implicit, preferred) then
  `DRM_FORMAT_MOD_LINEAR`; Vulkan cannot import the implicit modifier.
- `Frame`, `FrameBuffer`, `Plane`, `DrmFourcc` and `BufferLease` live in `rusplay-core` (`frame.rs`) so
  `rusplay-encode` needs no edge to capture; `rusplay-capture` re-exports them.
- A DMA-BUF `Frame` owns a duplicate of the plane fd (`FrameBuffer::DmaBuf { fd, modifier,
  planes, lease }`, one `Plane { offset, stride }` per `spa_data`, e.g. AMD DCC metadata planes).
  Every plane must live in the same buffer (compared by device and inode), otherwise
  `DmaBufPlanesSplit`. The PipeWire buffer stays dequeued for as long as the frame's
  `BufferLease` lives, so the compositor cannot render into it while anything reads it: dropping
  the frame (a newer one superseding it in the handoff, or the consumer letting go) sends a
  release command to the PipeWire thread, which requeues it. The frame can therefore cross
  threads and be kept for as long as its consumer needs. The handle is an address plus a serial
  from `HeldBuffers::hold`, and is only requeued on the PipeWire thread, only if no
  `remove_buffer` event dropped it meanwhile and the serial still matches. Frames built outside
  PipeWire (tests, synthetic sources) use `BufferLease::detached()`.
- `FrameBuffer::OpaqueFd { fd, allocation, size, dedicated, planes }` is never produced by
  capture: it is the host's GPU hand-off of exported Vulkan memory to NVENC (`host.md`), carried in
  `Frame` because that is the encoder input type. The host refuses one as a capture frame
  (`CaptureOpaqueFd`).
- `PipeWireCapture::shm_fallback` hands out a `ShmFallback` (a shared flag, usable without the
  `pipewire` feature). A consumer that cannot import the DMA-BUF frames calls `request`; the next
  `process` turns `prefer_dmabuf` off and calls `update_params` with the shared-memory
  `EnumFormat` only, so the producer renegotiates to shared memory for the rest of the stream.
- All unsafe PipeWire buffer access sits in `src/pipewire/raw.rs` (`DequeuedBuffer`, which requeues
  on drop, `borrow_fd`, `HeldBuffers`). The invariant it relies on: every pointer and fd it hands
  out belongs to a buffer that is dequeued for at least as long as the borrow.
- The consumer bounds how many buffers it keeps dequeued. The host keeps at most the handoff slot,
  its own mailbox, the frame being encoded and nothing else (it drops the previous retained frame
  before encoding the next), against the `DEFAULT_BUFFER_COUNT` of 4 the stream asks for.
- With `capture.cursor = "metadata"` (`StreamOptions::cursor_metadata`) the stream asks for
  `SPA_META_Cursor` sized for bitmaps up to 512×512. Every dequeued buffer, including the
  cursor-only ones and the stale ones drained before the newest, is parsed by
  `parse_cursor_meta` (pure, over the meta's bytes: id 0 is `CursorShape::Absent`, the cursor
  is not in this stream, which is how OBS reads it and how Mutter writes a cursor it does not
  show; a bitmap offset below the cursor struct means the shape is unchanged, a zero-sized bitmap
  means hidden). The bitmap is normalised to tightly packed RGBA8. `CursorTracker` keeps two
  reasons apart: an absent cursor is hidden at its last position until the next valid meta,
  whatever its shape field says, while a hidden sprite (a zero-sized bitmap, or a fully
  transparent one, which is how Wine and SDL hide the cursor of a game) stays hidden as the
  cursor moves until a visible bitmap arrives. It drops unchanged updates and strips a bitmap
  identical to the last one, so a bitmap travels only when it changes.
- Cursor updates have their own `latest` handoff, independent of frames, obtained with
  `CaptureSource::cursor_feed` (a cloneable `Subscriber<CursorUpdate>`, `None` for sources without
  cursor metadata). A newer update merges over an unconsumed one with `CursorUpdate::absorb`, which
  keeps the stale bitmap when the newer update has none, so a shape change is never lost to
  coalescing. On the wire the update becomes `rusplay_protocol::CursorMessage` on
  `StreamKind::Cursor`.
- `tests/live.rs` holds `#[ignore]`d tests. Five run an in-process PipeWire video source and
  capture from it (needs only a PipeWire daemon): shared memory, DMA-BUF (fake dma-bufs backed by
  temp files, exercising modifier fixation and lease release across a dozen frames) and the
  `prefer_dmabuf = false` path, plus cursor metadata (bitmap sent once, then position only) and
  colour: an undescribed source is captured as sRGB/BT.709 although the default stream prefers
  BT.709 (the drop flag at work), and a source describing PQ/BT.2020 is captured as that. One
  goes through the real portal picker.
- The cursor defaults to `Metadata` so it can be drawn on the client at full rate and without
  compression artefacts. `Embedded` and `Hidden` are options.
- Wine/Proton games need nothing special: they are Wayland (or XWayland) surfaces the
  compositor already composites.
