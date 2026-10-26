# Video encode
- [L] `encode-nvenc`: NVENC encoder with CUDA external-memory input
- [S] `encode-vaapi-rext-hardware`: run the VAAPI 4:2:2/4:4:4 paths (HEVC RExt profiles, AV1 Profile 1, the packed `YUYV`/`Y210`/`AYUV`/`Y410`/`Y412` uploads in `vaapi/format.rs`) and the AV1 packed frame header on a driver that consumes them verbatim (Intel iHD); radeonsi exposes none of the formats and rewrites the frame header

# Video decode
- [S] `decode-nvdec-hardware`: run the NVDEC backend on an NVIDIA GPU, which this machine lacks: `libcuda.so.1`/`libnvcuvid.so.1` loading and every hand-written struct against a real driver, H.264, HEVC (Main, Main10, Main12, 4:4:4 and, on GPUs whose caps report `NV16`/`P216`, 4:2:2) and AV1 fixtures decoding one frame per packet with display delay 0 and `CUVID_PKT_ENDOFPICTURE`, output planes matching ffmpeg's decode (the VAAPI tests' FNV-1a hashes), B-frame reordering and `flush` after `CUVID_PKT_ENDOFSTREAM` (and whether the parser accepts data after it), a mid-stream resolution change recreating the decoder, the `ChromaUnsupported`/`BitDepthUnsupported`/`ResolutionUnsupported` paths from `cuvidGetDecoderCaps`, and whether `ulErrorThreshold = 0` silently skips corrupted pictures where the client would rather resync

# Colour
- [S] `capture-hdr-metadata`: take mastering display and content light levels from the capture once PipeWire or the portal carries them (PipeWire 1.6 has no such metadata, so `HdrLevels::mastering` and `content_light` stay unset)
