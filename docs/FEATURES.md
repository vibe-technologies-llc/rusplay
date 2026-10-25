# Capture
- [M] `capture-pipewire`: consume the portal's PipeWire node into `Frame`s behind the `pipewire` feature
- [M, blocked by: capture-pipewire] `capture-dmabuf`: negotiate DMA-BUF with modifiers, falling back to shared memory
- [S, blocked by: capture-pipewire] `capture-cursor-metadata`: extract cursor metadata and forward it to the client
- [S] `capture-restore-token`: persist the portal restore token between host runs

# Scaling
- [M] `scale-gpu-tests`: headless wgpu tests comparing GPU output with a CPU reference of the kernels
- [M] `scale-polar-pass`: single-pass 2D EWA shader for the polar filters
- [S] `scale-antiringing`: clamp upscaled output to the local min/max by `antiringing` strength
- [S] `scale-sigmoid`: sigmoidal light transform around the upscale pass
- [M, blocked by: capture-dmabuf] `scale-dmabuf-import`: import capture DMA-BUFs into wgpu without a copy
- [M] `scale-color-convert`: RGB to YUV 4:2:0/4:2:2/4:4:4 conversion on the GPU with the chroma downscale filter

# Video encode
- [L, blocked by: scale-color-convert] `encode-vaapi`: VAAPI encoder for H.264, HEVC and AV1 with DMA-BUF input
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-svt-av1-log`: route SVT-AV1 log output through svt_av1_set_log_callback instead of stderr
- [S, blocked by: encode-vaapi] `encode-chroma-probe`: runtime probing of 4:2:2/4:4:4 support per device

# Video decode
- [L] `decode-vaapi`: VAAPI decoder that exports DMA-BUF frames
- [L] `decode-nvdec`: NVDEC decoder
- [M] `decode-rav1d`: software AV1 decode via rav1d

# Audio
- [M] `audio-capture`: PipeWire audio capture of the default sink monitor at 48 kHz float
- [M] `audio-playback`: PipeWire playback on the client with a minimal jitter buffer
- [M] `audio-flac`: FLAC encoder/decoder pair behind the `flac` feature
- [M] `audio-surround`: 5.1/7.1 Opus through opus-rs multistream
- [S] `audio-resample`: CPU resampling with rubato (or the Resonate resampler) when PipeWire cannot deliver 48 kHz

# Networking
- [M] `net-fec`: Reed-Solomon shard encode/decode for `FecScheme`
- [M] `net-reassembly`: fragment reassembly into frames with loss detection
- [M] `net-control`: acknowledged control channel carrying `ControlMessage`
- [M, blocked by: net-control] `net-pairing`: authenticated pairing and encrypted session keys
- [S] `net-discovery`: LAN host discovery

# Input
- [M] `input-libei`: host injection through the RemoteDesktop portal and libei
- [M] `input-gamepad`: virtual gamepads on the host
- [M, blocked by: app-ui-shell] `input-client-capture`: capture keyboard, pointer (locked/relative) and gamepads in the client window

# Sessions
- [L, blocked by: capture-pipewire, encode-vaapi, net-reassembly] `host-pipeline`: run capture → scale → encode → send in `HostSession`
- [L, blocked by: decode-vaapi, net-reassembly] `client-pipeline`: run receive → decode → scale → present in `ClientSession`

# Desktop app
- [M] `app-ui-shell`: gpui window on Wayland with the gpui git dependency pinned
- [M, blocked by: app-ui-shell, client-pipeline] `app-stream-view`: present the stream through the shared wgpu device
- [M, blocked by: app-ui-shell] `app-settings`: settings UI covering every `rusplay-config` option
