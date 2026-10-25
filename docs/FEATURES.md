# Scaling
- [M] `scale-dmabuf-import`: import capture DMA-BUFs into wgpu without a copy
- [S] `scale-polar-chroma`: polar (EWA) filters for the chroma downscale in `ColorConverter`, which rejects them today
- [M] `scale-hdr-transfer`: PQ and HLG encoding in `ColorConverter` for linear-light HDR input

# Video encode
- [L] `encode-vaapi`: VAAPI encoder for H.264, HEVC and AV1 with DMA-BUF input
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-svt-av1-log`: route SVT-AV1 log output through svt_av1_set_log_callback instead of stderr
- [S, blocked by: encode-vaapi] `encode-chroma-probe`: runtime probing of 4:2:2/4:4:4 support per device

# Video decode
- [L] `decode-vaapi`: VAAPI decoder that exports DMA-BUF frames
- [L] `decode-nvdec`: NVDEC decoder
- [M] `decode-rav1d`: software AV1 decode via rav1d

# Networking
- [M] `net-remembered-peers`: persist a pairing secret so a paired client reconnects without a new PIN

# Input
- [M] `input-libei`: host injection through the RemoteDesktop portal and libei
- [M] `input-gamepad`: virtual gamepads on the host
- [M, blocked by: app-ui-shell] `input-client-capture`: capture keyboard, pointer (locked/relative) and gamepads in the client window

# Sessions
- [L, blocked by: encode-vaapi] `host-pipeline`: run capture → scale → encode → send in `HostSession`
- [L, blocked by: decode-vaapi] `client-pipeline`: run receive → decode → scale → present in `ClientSession`

# Desktop app
- [M] `app-ui-shell`: gpui window on Wayland with the gpui git dependency pinned
- [M, blocked by: app-ui-shell, client-pipeline] `app-stream-view`: present the stream through the shared wgpu device
- [M, blocked by: app-ui-shell] `app-settings`: settings UI covering every `rusplay-config` option
