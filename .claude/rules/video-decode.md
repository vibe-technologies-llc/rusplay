---
paths:
  - "crates/rusplay-decode/**"
---

# Video decode

- `VideoDecoder` is the backend-neutral trait: one `EncodedFrame` (a whole access unit / temporal
  unit) in, at most one `DecodedFrame` out. Backends are VAAPI, NVDEC and software, each behind
  its own cargo feature, and `open` returns `BackendNotCompiled` when it is off. Decoders are
  not `Send`: a decoder lives on the thread that created it.
- The decoder backend and its options are `video.decoder` in `rusplay-config`
  (`VideoDecoderConfig`, tagged by `backend`: `vaapi`, `nvdec`, `software`).
- `DecodedFrame` carries the display `resolution`, the `PixelFormat` (chroma and bit depth) the
  stream was coded in, the input `timestamp` and a `DecodedBuffer`:
  - `DecodedBuffer::DmaBuf(DmaBufFrame)` from hardware decoders, for import into wgpu where
    `rusplay-scale` upscales and converts to the display format. `DmaBufFrame::image()` is a
    `DmaBufImage`: DRM `fourcc`, `modifier`, allocated `width`/`height`, the `objects` (an
    owned fd and size each) and the `planes` (object index, offset, pitch), which is exactly
    what `VK_EXT_image_drm_format_modifier` import needs. The `DmaBufFrame` is a lease: the
    buffer is not reused by the decoder until it is dropped, so it must outlive the GPU work
    that reads it. Importers dup the fd if the API takes ownership.
  - `DecodedBuffer::Cpu(CpuFrame)`, the software path only: three planes (Y, U, V) borrowed
    from the decoder's own picture without a copy. Each `CpuPlane` has `data`, `stride` in
    bytes, `width`/`height` in samples and `bytes_per_sample` (1 for 8-bit, 2 for 10/12-bit,
    whose samples are little-endian `u16`s holding the value in the low bits).
- Monochrome streams are rejected with `DecodeError::Monochrome`; there is no 4:0:0
  `ChromaSubsampling`.
- No decoder-side frame buffering beyond what the codec requires: a decoded frame is presented
  as soon as it exists. When one packet yields several pictures, the newest one wins.

## Software (`software` feature)

- AV1 through `rav1d` (pure Rust port of dav1d), driven through its dav1d C API
  (`dav1d_open`, `dav1d_send_data`, `dav1d_get_picture`), which is the only public interface it
  has. The unsafe calls are confined to `src/software.rs` and wrapped in owning types
  (`InputData`, `OwnedPicture`) that unref on drop.
- `max_frame_delay = 1` turns frame threading off so every packet yields its picture on the same
  call; `SoftwareDecoderConfig::threads` sets the tile/postfilter thread count (`None` lets
  rav1d use every logical CPU, at most 256).
- rav1d is built with `default-features = false` plus `bitdepth_8` and `bitdepth_16`. Its `asm`
  feature needs `nasm` at build time and is off; see `decode-rav1d-asm` in
  `docs/PERFORMANCE.md`.
- H.264/HEVC software decode would need a C-backed crate and is not implemented: the software
  backend decodes AV1 only.

## Test fixtures

- `tests/data/` holds 8-frame 256x144 clips made by `tests/data/generate.sh` with ffmpeg:
  AV1 in IVF (8-bit and 10-bit 4:2:0, 8-bit 4:4:4), H.264 and HEVC as Annex B with access unit
  delimiters (the tests split on them). `tests/common` has the IVF and Annex B splitters.
