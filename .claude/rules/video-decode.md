---
paths:
  - "crates/rusplay-decode/**"
---

# Video decode

- `VideoDecoder` is the backend-neutral trait. Backends are VAAPI, NVDEC and software, each
  behind its own cargo feature, and `open` returns `BackendNotCompiled` when it is off.
- Hardware decoders hand frames out as DMA-BUF (`DecodedBuffer::DmaBuf`) for import into wgpu,
  where `rusplay-scale` upscales and converts to the display format. CPU frames are the
  software path only.
- Software decode prefers pure Rust (`rav1d` for AV1). H.264/HEVC software decode needs a
  C-backed crate and is feature gated.
- No decoder-side frame buffering beyond what the codec requires: a decoded frame is presented
  as soon as it exists.
