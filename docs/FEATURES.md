# Scaling
- [S] `scale-dmabuf-sync`: wait for a capture DMA-BUF's implicit fence (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE` imported as a Vulkan semaphore) or PipeWire's explicit sync timeline before reading it; only VAAPI frames are synchronised today (`vaSyncSurface`); wgpu 30 does not enable `VK_KHR_external_semaphore_fd`, so the device must be opened through `wgpu::hal::vulkan::Adapter::open_with_callback` adding it (replacing `device_descriptor` for every caller) before `hal::vulkan::Queue::add_wait_semaphore` can gate the next submit on the imported sync file

# Video encode
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [L] `decode-nvdec`: NVDEC decoder

# Sessions
- [M] `host-input`: read `StreamKind::Input` datagrams (`InputPacket`, `InputSequence`, `InputEvent::decode`) in `HostSession` into a `SplitSink` of `LibeiInjector` and `UinputGamepads` from one combined RemoteDesktop + ScreenCast portal session, call `UinputGamepads::poll_rumble` from the input loop and send each `RumbleCommand` as `ControlMessage::Rumble`; the contract is in `.claude/rules/input.md`
- [M] `host-dmabuf-capture`: convert DMA-BUF capture frames on the GPU and let PipeWire negotiate them again; the host asks for shared memory and returns `DmaBufCaptureUnsupported` today

# Colour
- [M] `capture-hdr-negotiation`: offer colour primaries and transfer in the capture `EnumFormat` so HDR-capable compositors pick them, and take mastering display and content light levels once PipeWire or the portal carries them (PipeWire 1.6 has no such metadata, so `mastering` and `content_light` stay unset)
- [S] `client-color-output-follow`: track the output the stream window is on (`wl_surface.enter` on the child surface, matched to `query_outputs` by `wl_output` name), and re-query its image description and re-choose the surface output when it or the output's description changes; `StreamWindow::attach` queries the first output once today
