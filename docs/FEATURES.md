# Video encode
- [S] `encode-nvenc-hardware`: run the NVENC backend on an NVIDIA GPU (driver R550 or newer, NVENC API 12.2); nothing of it has touched a driver yet. Check that the hardware tests in `nvenc/tests.rs` pass (capabilities agree with `open`, keyframe on the first frame and on request, every rate-control mode, the first frame fixing the resolution, AV1 10-bit HDR10 colour and metadata through rav1d) and that H.264/HEVC SEI and VUI decode on another machine; that `cuImportExternalMemory` with `CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD` accepts a linear `NV12`/`P010`/`YU24`/`Q410` DMA-BUF from PipeWire or udmabuf (if CUDA refuses foreign DMA-BUFs, import through Vulkan or EGL and hand CUDA that memory instead); that `nvEncReconfigureEncoder` applies `set_bitrate` without an IDR; that intra refresh (count = `intra-refresh-frames`, period one more) keeps the stream recoverable after loss; and that `split-encode` and `multipass` params are accepted on a GPU with two NVENC engines
- [M] `encode-nvenc-sdk13`: move the NVENC FFI to Video Codec SDK 13, for 4:2:2 (`NV16`/`P210` input, H.264/HEVC 4:2:2 on Blackwell), 10-bit H.264 and the native mastering-display and content-light fields in place of the SEI/OBU payloads; 12.2 has none of them, so 4:2:2 (the default stream chroma) is `ChromaUnsupported` on NVENC today
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [L] `decode-nvdec`: NVDEC decoder

# Colour
- [S] `capture-hdr-metadata`: take mastering display and content light levels from the capture once PipeWire or the portal carries them (PipeWire 1.6 has no such metadata, so `HdrLevels::mastering` and `content_light` stay unset)
