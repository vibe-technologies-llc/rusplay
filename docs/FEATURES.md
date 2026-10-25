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
- [L] `host-pipeline`: run capture → scale → encode → send in `HostSession`
- [M] `session-stream-format`: carry codec, resolution, pixel format, `ColorSpace` and HDR metadata in the handshake; the client decodes with its own `video.codec` and labels frames with its own `video.color` today
- [M] `client-audio`: receive `StreamKind::Audio`, decode on a codec thread with PLC/FEC concealment and play through `AudioPlayback` in `ClientStream`
- [S] `client-cursor`: consume `StreamKind::Cursor` in the receive stage and hand cursor position and image to the app beside `PresentedFrame`
- [M, blocked by: scale-dmabuf-import] `client-dmabuf-present`: present VAAPI DMA-BUF frames (`NV12`/`P010`); the present stage returns `DmaBufPresentUnsupported` today, so only the software decoder streams

# Desktop app
- [M] `app-stream-view`: present the stream through the shared wgpu device

# Colour
- [M] `color-signalling`: carry the colour description and HDR metadata in the protocol's stream description and in every encoder's VUI / AV1 `color_config` / HDR SEI and metadata OBUs, and report the bitstream's description from the decoders
- [M] `capture-hdr-negotiation`: offer colour primaries and transfer in the capture `EnumFormat` so HDR-capable compositors pick them, and take mastering display and content light levels once PipeWire or the portal carries them (PipeWire 1.6 has no such metadata, so `mastering` and `content_light` stay unset)
- [M] `client-color-pipeline`: run decode → linearise → tone map → gamut map → scale → display encode with dithering in the client, fused or baked into a 3D LUT
