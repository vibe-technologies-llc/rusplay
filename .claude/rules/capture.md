---
paths:
  - "crates/rusplay-capture/**"
---

# Capture

- Capture goes through the xdg-desktop-portal ScreenCast interface (`ashpd`, tokio runtime,
  `screencast` feature) and then the PipeWire node the portal returns. No compositor-specific
  protocols (wlr-screencopy, ext-image-copy-capture) until the portal path is complete, and no X11.
- The portal session asks for persistence (`PersistMode::ExplicitlyRevoked`) when
  `capture.persist_session` is on, and the restore token is handed back so the host can store it
  and skip the picker on the next run.
- PipeWire consumption lives behind the `pipewire` feature (`src/pipewire/`). `PipeWireCapture`
  connects to the portal's remote fd and node id and implements `CaptureSource`. The PipeWire main
  loop runs on its own `rusplay-pipewire` thread; setup errors come back synchronously from
  `connect`, later ones (stream error state, a malformed buffer) are stored and returned by the
  next `next_frame` once the handoff closes.
- Frames cross threads through `handoff::latest`, a single-slot mailbox: a new frame replaces an
  unconsumed one (counted by `superseded_frames`), so the consumer always gets the newest frame
  and nothing queues. `process` also drains PipeWire's queue down to the newest buffer.
- The format pods, the buffer → `Frame` conversion and the SPA ↔ DRM fourcc table are pure
  functions in `format.rs` and `convert.rs`, unit tested without a daemon. SPA names formats in
  byte order and DRM in little-endian word order, so SPA `BGRx` is DRM `XR24`. Formats are
  offered in `StreamOptions::formats` order; 10-bit (`XR30` family) goes first when
  `video.pixel_format.depth` is above 8.
- Shared-memory frames are copied out of the mapped buffer (`MAP_BUFFERS`) as `stride × height`
  bytes from the chunk offset, and the buffer is requeued at once. Buffers whose chunk is empty or
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
  modifiers it can import. The default is `DEFAULT_MODIFIERS`: `DRM_FORMAT_MOD_INVALID` (implicit,
  preferred) then `DRM_FORMAT_MOD_LINEAR`.
- A DMA-BUF `Frame` owns a duplicate of the plane fd (`FrameBuffer::DmaBuf { fd, modifier,
  planes }`, one `Plane { offset, stride }` per `spa_data`, e.g. AMD DCC metadata planes). Every
  plane must live in the same buffer (compared by device and inode), otherwise
  `DmaBufPlanesSplit`. The PipeWire buffer stays dequeued while its frame is in the handoff or held
  by the consumer, so the compositor cannot render into it: it is requeued when a newer frame
  supersedes it, or when the consumer next calls any `next_frame` variant (the contents of a
  DMA-BUF frame are valid until then). The buffer handle crosses threads as an address and is only
  requeued on the PipeWire thread, and only if no `remove_buffer` event dropped it meanwhile.
- All unsafe PipeWire buffer access sits in `src/pipewire/raw.rs` (`DequeuedBuffer`, which requeues
  on drop, `borrow_fd`, `HeldBuffers`). The invariant it relies on: every pointer and fd it hands
  out belongs to a buffer that is dequeued for at least as long as the borrow.
- `tests/live.rs` holds `#[ignore]`d tests. Three run an in-process PipeWire video source and
  capture from it (needs only a PipeWire daemon): shared memory, DMA-BUF (fake dma-bufs backed by
  temp files, exercising modifier fixation and buffer hold/release across a dozen frames) and the
  `prefer_dmabuf = false` path. One goes through the real portal picker.
- The cursor defaults to `Metadata` so it can be drawn on the client at full rate and without
  compression artefacts. `Embedded` and `Hidden` are options.
- Wine/Proton games need nothing special: they are Wayland (or XWayland) surfaces the
  compositor already composites.
