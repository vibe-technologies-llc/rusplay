# Video encode
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [L] `decode-nvdec`: NVDEC decoder

# Input
- [M, blocked by: app-stream-view] `input-app-attach`: attach `rusplay_input::wayland::WaylandCapture` to the stream window (display and surface pointers from gpui's raw window handles), feed it `ClientStream::input()`, call `set_region` with the letterboxed picture rectangle on every resize, hide the local cursor while engaged, show the release chord and engaged state in the UI, and call `release` when the window loses focus or the stream ends

# Sessions
- [M] `host-input`: read `StreamKind::Input` datagrams (`InputPacket`, `InputSequence`, `InputEvent::decode`) in `HostSession` into a `SplitSink` of `LibeiInjector` and `UinputGamepads` from one combined RemoteDesktop + ScreenCast portal session, call `UinputGamepads::poll_rumble` from the input loop and send each `RumbleCommand` as `ControlMessage::Rumble`; the contract is in `.claude/rules/input.md`

# Desktop app
- [M] `app-stream-view`: present the stream through the shared wgpu device, enable the client's `pipewire` and `flac` features for audio, and draw `ClientStream::cursor` over the picture

# Colour
- [S] `capture-hdr-metadata`: take mastering display and content light levels from the capture once PipeWire or the portal carries them (PipeWire 1.6 has no such metadata, so `HdrLevels::mastering` and `content_light` stay unset)
- [M] `client-color-pipeline`: run decode → linearise → tone map → gamut map → scale → display encode with dithering in the client, fused or baked into a 3D LUT
