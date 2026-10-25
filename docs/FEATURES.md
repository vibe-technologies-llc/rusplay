# Scaling
- [M] `scale-dmabuf-import`: import capture DMA-BUFs into wgpu without a copy
- [S] `scale-polar-chroma`: polar (EWA) filters for the chroma downscale in `ColorConverter`, which rejects them today
- [M] `scale-hdr-transfer`: PQ and HLG encoding in `ColorConverter` for linear-light HDR input

# Video encode
- [L] `encode-vaapi`: VAAPI encoder for H.264, HEVC and AV1 with DMA-BUF input
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S, blocked by: encode-vaapi] `encode-chroma-probe`: runtime probing of 4:2:2/4:4:4 support per device

# Video decode
- [L] `decode-nvdec`: NVDEC decoder
- [S] `decode-reorder`: hold frames for display order when a stream signals reordering (B-frames); the VAAPI backend outputs in decode order, which is display order for Rusplay's own encoders
- [S] `decode-vaapi-interlaced`: field-coded H.264 pictures in the VAAPI backend (returns `InterlacedUnsupported` today)

# Input
- [M] `input-gamepad-rumble`: carry uinput force-feedback effects from the host's virtual pads to the client's physical gamepad
- [M] `input-client-capture`: capture keyboard, pointer (locked/relative) and gamepads in the client window

# Sessions
- [L, blocked by: encode-vaapi] `host-pipeline`: run capture → scale → encode → send in `HostSession`
- [M] `session-stream-format`: carry codec, resolution, pixel format, `ColorSpace` and HDR metadata in the handshake; the client decodes with its own `video.codec` and labels frames with its own `video.color` today
- [M] `client-audio`: receive `StreamKind::Audio`, decode on a codec thread with PLC/FEC concealment and play through `AudioPlayback` in `ClientStream`
- [S] `client-cursor`: consume `StreamKind::Cursor` in the receive stage and hand cursor position and image to the app beside `PresentedFrame`
- [M, blocked by: scale-dmabuf-import] `client-dmabuf-present`: present VAAPI DMA-BUF frames (`NV12`/`P010`); the present stage returns `DmaBufPresentUnsupported` today, so only the software decoder streams

# Desktop app
- [M] `app-stream-view`: present the stream through the shared wgpu device
