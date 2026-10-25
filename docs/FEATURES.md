# Scaling
- [M] `scale-dmabuf-import`: import capture DMA-BUFs into wgpu without a copy

# Video encode
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [L] `decode-nvdec`: NVDEC decoder

# Input
- [M] `input-gamepad-rumble`: carry uinput force-feedback effects from the host's virtual pads to the client's physical gamepad
- [M] `input-client-capture`: capture keyboard, pointer (locked/relative) and gamepads in the client window

# Sessions
- [M, blocked by: scale-dmabuf-import] `client-dmabuf-present`: present VAAPI DMA-BUF frames (`NV12`/`P010`); the present stage returns `DmaBufPresentUnsupported` today, so only the software decoder streams

- [M] `host-audio`: capture with `AudioCapture`, encode on a codec thread and send `StreamKind::Audio` from the host session
- [M] `host-input`: receive input events on the control path and inject them through one combined RemoteDesktop + ScreenCast portal session (`LibeiInjector`, `UinputGamepads`)
- [S] `host-cursor`: send the capture's `cursor_feed` as latest-wins `CursorMessage`s on `StreamKind::Cursor`
- [M, blocked by: scale-dmabuf-import] `host-dmabuf-capture`: convert DMA-BUF capture frames on the GPU and let PipeWire negotiate them again; the host asks for shared memory and returns `DmaBufCaptureUnsupported` today
- [M] `host-ten-bit-capture`: accept the 10-bit `XR30`/`XB30` capture formats, which have no sRGB texture view, by linearising them in a shader before scaling
- [S] `hostd-discovery`: run `DiscoveryResponder` in `rusplay-hostd` so clients find the host without an address

# Desktop app
- [M] `app-stream-view`: present the stream through the shared wgpu device, enable the client's `pipewire` and `flac` features for audio, and draw `ClientStream::cursor` over the picture

# Colour
- [M] `capture-color-description`: read the captured surface's colour description and HDR metadata from PipeWire/portal metadata instead of assuming sRGB, so an HDR game arrives as BT.2020 PQ 10-bit
- [M] `client-display-color`: query the client output's image description through `wp_color_management_v1` and resolve it against the `color.target` overrides
- [M, blocked by: client-display-color] `client-color-pipeline`: run decode → linearise → tone map → gamut map → scale → display encode with dithering in the client, fused or baked into a 3D LUT
