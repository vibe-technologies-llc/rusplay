# Scaling
- [M] `scale-dmabuf-import`: import capture DMA-BUFs into wgpu without a copy
- [S] `scale-polar-chroma`: polar (EWA) filters for the chroma downscale in `ColorConverter`, which rejects them today
- [M] `scale-hdr-transfer`: PQ and HLG encoding in `ColorConverter` for linear-light HDR input

# Video encode
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [L] `decode-nvdec`: NVDEC decoder
- [S] `decode-reorder`: hold frames for display order when a stream signals reordering (B-frames); the VAAPI backend outputs in decode order, which is display order for Rusplay's own encoders
- [S] `decode-vaapi-interlaced`: field-coded H.264 pictures in the VAAPI backend (returns `InterlacedUnsupported` today)

# Input
- [M] `input-gamepad-rumble`: carry uinput force-feedback effects from the host's virtual pads to the client's physical gamepad
- [M] `input-client-capture`: capture keyboard, pointer (locked/relative) and gamepads in the client window

# Sessions
- [L] `host-pipeline`: run capture → scale → encode → send in `HostSession`
- [L] `client-pipeline`: run receive → decode → scale → present in `ClientSession`

# Desktop app
- [M, blocked by: client-pipeline] `app-stream-view`: present the stream through the shared wgpu device
