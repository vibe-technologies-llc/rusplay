# Scaling
- [S] `scale-dmabuf-swizzled-rgb`: import the RGB DMA-BUF formats wgpu has no texture format for (`XR30`/`AR30`, which compositors offer first for 10-bit, and `BX24`/`RX24`/`RX30`/`BX30`) by passing a channel order to the scaler and colour converter source reads; `DmaBufImporter` returns `DmaBufFormatUnsupported` for them today
- [S] `scale-dmabuf-sync`: wait for a capture DMA-BUF's implicit fence (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE` imported as a Vulkan semaphore) or PipeWire's explicit sync timeline before reading it; only VAAPI frames are synchronised today (`vaSyncSurface`)

# Video encode
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [L] `decode-nvdec`: NVDEC decoder

# Sessions
- [M] `host-input`: read `StreamKind::Input` datagrams (`InputPacket`, `InputSequence`, `InputEvent::decode`) in `HostSession` into a `SplitSink` of `LibeiInjector` and `UinputGamepads` from one combined RemoteDesktop + ScreenCast portal session, call `UinputGamepads::poll_rumble` from the input loop and send each `RumbleCommand` as `ControlMessage::Rumble`; the contract is in `.claude/rules/input.md`
- [M] `host-dmabuf-capture`: convert DMA-BUF capture frames on the GPU and let PipeWire negotiate them again; the host asks for shared memory and returns `DmaBufCaptureUnsupported` today
- [M] `host-ten-bit-capture`: accept the 10-bit `XR30`/`XB30` capture formats, which have no sRGB texture view, by linearising them in a shader before scaling
- [M] `host-source-color`: linearise and map each capture frame from its `Frame::color` (transfer, primaries, HDR levels) instead of assuming sRGB in `GpuPipeline`, reconfiguring the mapper when the description changes

# Colour
- [M] `capture-hdr-negotiation`: offer colour primaries and transfer in the capture `EnumFormat` so HDR-capable compositors pick them, and take mastering display and content light levels once PipeWire or the portal carries them (PipeWire 1.6 has no such metadata, so `mastering` and `content_light` stay unset)
- [M] `client-color-pipeline`: run decode → linearise → tone map → gamut map → scale → display encode with dithering in the client, fused or baked into a 3D LUT
