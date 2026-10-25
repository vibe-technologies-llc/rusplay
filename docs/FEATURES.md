# Scaling
- [S] `scale-dmabuf-swizzled-rgb`: import the RGB DMA-BUF formats wgpu has no texture format for (`XR30`/`AR30`, which compositors offer first for 10-bit, and `BX24`/`RX24`/`RX30`/`BX30`) by passing a channel order to the scaler and colour converter source reads; `DmaBufImporter` returns `DmaBufFormatUnsupported` for them today
- [S] `scale-dmabuf-sync`: wait for a capture DMA-BUF's implicit fence (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE` imported as a Vulkan semaphore) or PipeWire's explicit sync timeline before reading it; only VAAPI frames are synchronised today (`vaSyncSurface`)

# Video encode
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [L] `decode-nvdec`: NVDEC decoder

# Input
- [M] `input-gamepad-rumble`: carry uinput force-feedback effects from the host's virtual pads to the client's physical gamepad
- [M] `input-client-capture`: capture keyboard, pointer (locked/relative) and gamepads in the client window

# Sessions
- [L] `host-pipeline`: run capture → scale → encode → send in `HostSession`
- [M] `session-stream-format`: carry codec, resolution, pixel format, `ColorSpace` and HDR metadata in the handshake; the client decodes with its own `video.codec` and labels frames with its own `video.color` today
- [M] `client-audio`: receive `StreamKind::Audio`, decode on a codec thread with PLC/FEC concealment and play through `AudioPlayback` in `ClientStream`
- [S] `client-cursor`: consume `StreamKind::Cursor` in the receive stage and hand cursor position and image to the app beside `PresentedFrame`

# Desktop app
- [M] `app-stream-view`: present the stream through the shared wgpu device

# Colour
- [M] `color-signalling`: carry the colour description and HDR metadata in the protocol's stream description and in every encoder's VUI / AV1 `color_config` / HDR SEI and metadata OBUs, and report the bitstream's description from the decoders
- [M] `capture-color-description`: read the captured surface's colour description and HDR metadata from PipeWire/portal metadata instead of assuming sRGB, so an HDR game arrives as BT.2020 PQ 10-bit
- [M] `client-display-color`: query the client output's image description through `wp_color_management_v1` and resolve it against the `color.target` overrides
- [M, blocked by: client-display-color] `client-color-pipeline`: run decode → linearise → tone map → gamut map → scale → display encode with dithering in the client, fused or baked into a 3D LUT
