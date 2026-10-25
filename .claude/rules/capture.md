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
- `tests/live.rs` holds `#[ignore]`d tests: one runs an in-process PipeWire video source and
  captures from it (needs only a PipeWire daemon), one goes through the real portal picker.
- Frames are DMA-BUF whenever the compositor offers them (`capture.prefer_dmabuf`), so they reach
  the GPU scaler and hardware encoder without a CPU copy. Shared memory is the fallback, not the
  default.
- The cursor defaults to `Metadata` so it can be drawn on the client at full rate and without
  compression artefacts. `Embedded` and `Hidden` are options.
- Wine/Proton games need nothing special: they are Wayland (or XWayland) surfaces the
  compositor already composites.
