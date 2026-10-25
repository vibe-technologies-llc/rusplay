# Scaling
- [M] `scale-dmabuf-import`: import capture DMA-BUFs into wgpu without a copy
- [S] `scale-polar-chroma-422`: polar (EWA) chroma filters for 4:2:2 in `ColorConverter`, which rejects them because a 2D kernel would blur the unsubsampled vertical axis

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
- [L] `client-pipeline`: run receive → decode → scale → present in `ClientSession`

# Desktop app
- [M, blocked by: client-pipeline] `app-stream-view`: present the stream through the shared wgpu device

# Colour
- [M] `color-model`: extend the `rusplay-core` colour types to the model in `.claude/rules/color.md`: chromaticity-based primaries (BT.709, Display P3, DCI-P3, BT.2020, BT.601, custom) with derived RGB↔XYZ and Bradford adaptation, the full transfer set, separate Y'CbCr matrix, and ST 2086 / MaxCLL HDR metadata
- [M, blocked by: color-model] `color-signalling`: carry the colour description and HDR metadata in the protocol's stream description and in every encoder's VUI / AV1 `color_config` / HDR SEI and metadata OBUs, and report the bitstream's description from the decoders
- [M, blocked by: color-model] `capture-color-description`: read the captured surface's colour description and HDR metadata from PipeWire/portal metadata instead of assuming sRGB, so an HDR game arrives as BT.2020 PQ 10-bit
- [M, blocked by: color-model] `client-display-color`: query the client output's image description through `wp_color_management_v1`, with config overrides for every field
- [L, blocked by: color-model] `color-gamut-map`: GPU gamut mapping between any primaries, perceptual by default (hue- and lightness-preserving soft chroma compression in ICtCp/Oklch), plus relative, absolute, saturation, desaturate and highlight modes
- [L, blocked by: color-model] `color-tone-map`: GPU tone mapping to a lower peak or SDR (BT.2390 EETF default, BT.2446a, spline, Reinhard, clip) with optional dynamic peak detection
- [M, blocked by: color-gamut-map, color-tone-map, client-display-color, client-pipeline] `client-color-pipeline`: run decode → linearise → tone map → gamut map → scale → display encode with dithering in the client, fused or baked into a 3D LUT
