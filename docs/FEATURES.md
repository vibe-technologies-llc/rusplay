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
- [S] `decode-vaapi-interlaced`: field-coded H.264 pictures in the VAAPI backend (returns `InterlacedUnsupported` today)

# Input
- [M] `input-gamepad-rumble`: carry uinput force-feedback effects from the host's virtual pads to the client's physical gamepad
- [M] `input-client-capture`: capture keyboard, pointer (locked/relative) and gamepads in the client window

# Sessions
- [L, blocked by: encode-vaapi] `host-pipeline`: run capture → scale → encode → send in `HostSession`
- [L] `client-pipeline`: run receive → decode → scale → present in `ClientSession`

# Desktop app
- [M, blocked by: client-pipeline] `app-stream-view`: present the stream through the shared wgpu device
