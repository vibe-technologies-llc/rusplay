# Scaling
- [S] `scale-dmabuf-swizzled-rgb`: import the RGB DMA-BUF formats wgpu has no texture format for (`XR30`/`AR30`, which compositors offer first for 10-bit, and `BX24`/`RX24`/`RX30`/`BX30`) by passing a channel order to the scaler and colour converter source reads; `DmaBufImporter` returns `DmaBufFormatUnsupported` for them today
- [S] `scale-dmabuf-sync`: wait for a capture DMA-BUF's implicit fence (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE` imported as a Vulkan semaphore) or PipeWire's explicit sync timeline before reading it; only VAAPI frames are synchronised today (`vaSyncSurface`)

# Video encode
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [L] `decode-nvdec`: NVDEC decoder

# Input
- [M, blocked by: app-stream-view] `input-app-attach`: attach `rusplay_input::wayland::WaylandCapture` to the stream window (display and surface pointers from gpui's raw window handles), feed it `ClientStream::input()`, call `set_region` with the letterboxed picture rectangle on every resize, hide the local cursor while engaged, show the release chord and engaged state in the UI, and call `release` when the window loses focus or the stream ends

# Sessions
- [M] `host-dmabuf-capture`: convert DMA-BUF capture frames on the GPU and let PipeWire negotiate them again; the host asks for shared memory and returns `DmaBufCaptureUnsupported` today
- [M] `host-ten-bit-capture`: accept the 10-bit `XR30`/`XB30` capture formats, which have no sRGB texture view, by linearising them in a shader before scaling
- [M] `host-source-color`: linearise and map each capture frame from its `Frame::color` (transfer, primaries, HDR levels) instead of assuming sRGB in `GpuPipeline`, reconfiguring the mapper when the description changes

# Desktop app
- [M] `app-stream-view`: present the stream through the shared wgpu device, enable the client's `pipewire` and `flac` features for audio, and draw `ClientStream::cursor` over the picture

# Colour
- [M] `capture-hdr-negotiation`: offer colour primaries and transfer in the capture `EnumFormat` so HDR-capable compositors pick them, and take mastering display and content light levels once PipeWire or the portal carries them (PipeWire 1.6 has no such metadata, so `mastering` and `content_light` stay unset)
- [M] `client-color-pipeline`: run decode → linearise → tone map → gamut map → scale → display encode with dithering in the client, fused or baked into a 3D LUT
