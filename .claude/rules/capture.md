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
- Frames are DMA-BUF whenever the compositor offers them (`capture.prefer_dmabuf`), so they reach
  the GPU scaler and hardware encoder without a CPU copy. Shared memory is the fallback, not the
  default.
- The cursor defaults to `Metadata` so it can be drawn on the client at full rate and without
  compression artefacts. `Embedded` and `Hidden` are options.
- Wine/Proton games need nothing special: they are Wayland (or XWayland) surfaces the
  compositor already composites.
