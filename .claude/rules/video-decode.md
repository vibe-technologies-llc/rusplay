---
paths:
  - "crates/rusplay-decode/**"
---

# Video decode

- `VideoDecoder` is the backend-neutral trait: one `EncodedFrame` (a whole access unit / temporal
  unit) in, at most one `DecodedFrame` out, plus `flush` for the frames a reordering stream
  still holds. Backends are VAAPI, NVDEC and software, each behind
  its own cargo feature. `rusplay_decode::open(codec, &VideoDecoderConfig)` dispatches on the
  config's backend and returns `BackendNotCompiled` when that feature is off (always for NVDEC,
  which has no backend yet) and `CodecUnsupported` when the backend cannot decode the codec.
  The backend types (`VaapiDecoder`, `Rav1dDecoder`) are public too. Decoders are not `Send`: a
  decoder lives on the thread that created it.
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
  as soon as it exists. When one packet yields several pictures, the newest one wins. Frames
  come out in display order. A stream that signals reordering (B-frames) is held for exactly
  its reorder depth, so `decode` returns `None` for the first packets and then one frame per
  packet; a stream without reordering, which is what Rusplay's encoders produce, comes out
  one-in-one-out with no added delay. `VideoDecoder::flush` returns the frames still held,
  oldest in display order, for the end of a stream; an IDR or an `mmco5` picture (H.264), an
  IRAP with `NoRaslOutputFlag` (HEVC) releases the previous sequence's held frames ahead of
  itself, and they leave over the following calls while the new sequence refills its own
  delay, so the depth never grows.

## Software (`software` feature)

- AV1 through `rav1d` (pure Rust port of dav1d), driven through its dav1d C API
  (`dav1d_open`, `dav1d_send_data`, `dav1d_get_picture`), which is the only public interface it
  has. The unsafe calls are confined to `src/software.rs` and wrapped in owning types
  (`InputData`, `OwnedPicture`) that unref on drop.
- `max_frame_delay = 1` turns frame threading off so every packet yields its picture on the same
  call; `SoftwareDecoderConfig::threads` sets the tile/postfilter thread count (`None` lets
  rav1d use every logical CPU, at most 256).
- rav1d is built with `default-features = false` plus `bitdepth_8` and `bitdepth_16`. Its `asm`
  feature (hand-written SIMD, several times faster) is behind `software-asm`, which implies
  `software` and is forwarded as `rusplay-client/software-asm`. It needs `nasm` at build time on
  x86_64, so `--all-features` builds need it installed; the default build stays free of it.
- `DecodedFrame::resolution` is the AV1 render size from the frame header, clamped per dimension
  to the coded picture, because an encoder that pads the frame (radeonsi codes 402 wide as 408
  and signals 402) must not decode wider than it was captured. The `CpuPlane`s report that
  cropped `width`/`height` over the full coded buffer. A render size larger than the coded one
  (AV1 spatial resize) is not scaled up: the coded picture is returned.
- H.264/HEVC software decode would need a C-backed crate and is not implemented: the software
  backend decodes AV1 only.

## VAAPI (`vaapi` feature)

- `VaapiDecoder::new(codec, &VaapiDecoderConfig)` opens `video.decoder.device` (default
  `/dev/dri/renderD128`) with `vaGetDisplayDRM`, and fails with `CodecUnsupported` when the
  driver has no VLD entrypoint for the codec's base profile (H.264 Constrained Baseline/Main/
  High, HEVC Main, AV1 Profile 0).
- libva is reached through bindgen bindings generated in `build.rs` from the system headers
  (`src/vaapi/ffi.rs`); `src/vaapi/va.rs` wraps display, config/context (`Session`), buffers and
  surfaces in owning types, and is the only place with libva calls besides the parameter-buffer
  unions. Errors are `DecodeError::Va { call, status }`.
- Parsing uses the `cros-codecs` parsers (`codec::h264`, `codec::h265`, `codec::av1`). The
  decoding processes are Rusplay's own, one module per codec, because the `cros-codecs`
  stateless decoders finish a picture only when the next one starts and output H.264 only once
  the DPB is full, which is a frame or more of latency, and their VAAPI backend cannot be built
  here (see `.claude/rules/dependencies.md`):
  - `h264.rs` with `h264/dpb.rs`: frame, field (PAFF) and MBAFF pictures, picture order count
    types 0/1/2, sliding-window and MMCO 1-6 reference marking, P/B list initialisation and
    `ref_pic_list_modification`, explicit weighted prediction, scaling matrices (zig-zag to
    raster). A picture is submitted at the end of its packet. The reorder depth is the VUI's
    `max_num_reorder_frames` (capped by `max_dec_frame_buffering`); without
    `bitstream_restriction` it is 0 for Baseline and picture order count type 2, and otherwise
    learned: it starts at 0 so streams that never reorder pay nothing, and each picture that
    arrives after a later one was already released is dropped and raises the depth by one, up to
    the level's DPB size. The spec's inference (`MaxDpbFrames`) is deliberately not used, since a
    VUI-less encoder without B-frames would then cost a DPB of latency.
    Field pictures: the DPB keeps a `Reference` per frame with a marking (unused, short-term,
    long-term) and a picture order count per field, so a frame can have one reference field.
    Both fields of a frame decode into one surface: a field pairs with the open first field when
    it has the opposite parity, the same `frame_num` and is not an IDR, and the frame is output
    (through the reorder queue) once the second field is decoded, whether the fields arrive in
    one packet or two. A first field followed by anything else is output alone, with the other
    field's lines undefined, and `flush` outputs a trailing one. Field reference lists follow
    8.2.4.2.5 (alternating parity starting with the current field's, missing fields skipped),
    field `PicNum`/`LongTermPicNum` arithmetic drives `ref_pic_list_modification` and MMCO 1-3,
    and the second field of a reference frame skips the sliding window. `ReferenceFrames` carries
    `TOP_FIELD`/`BOTTOM_FIELD` flags for frames with one marked field and the list entries carry
    the field they name. MBAFF frames are frame pictures to the decoder; the driver handles the
    field macroblock pairs. Mesa's VCN builds the reference lists itself from this DPB state, so
    the field tests double as a check of the driver against the spec.
  - `hevc.rs`: POC from the previous TemporalId-0 picture, the short- and long-term RPS (from the
    SPS or the slice), RefPicList construction with `list_entry` modification, dependent slice
    segments, RASL skipping after a random-access IRAP, range-extension buffers
    (`VAPictureParameterBufferHEVCExtension`) for the 4:2:2/4:4:4/12-bit profiles. The reorder
    depth is `sps_max_num_reorder_pics` of the highest sub-layer, always present in an SPS;
    `sps_max_latency_increase` is not applied, so a stream relying on it for earlier output waits
    for the reorder depth instead. Pictures with `pic_output_flag` 0 are decoded but not held.
  - `av1.rs`: OBUs from the packet, the eight reference slots, `show_existing_frame`, tile
    groups (a frame is submitted as soon as its last tile arrives, so the parser sees the updated
    reference state for the next header), film grain into a separate display surface. AV1 output
    order is the order frames are shown, so nothing is held: a hidden frame produces no output
    and `show_existing_frame` outputs the referenced slot when it is shown.
  - `reorder.rs`: the codec-independent output queue keyed by picture order count that the H.264
    and HEVC decoders push into. Its depth also sizes the surface pool (`reorder_frames` in
    `StreamFormat`), since held frames keep their surfaces busy.
- The stream format (VA profile, chroma, bit depth, coded size) is read from the SPS / sequence
  header; a change rebuilds the `Session` and surface pool. A profile or RT format the driver
  lacks is `ChromaUnsupported` (chroma above 4:2:0), `BitDepthUnsupported` (more than 8 bits) or
  `ProfileUnsupported`, checked with `vaGetConfigAttributes`, never downgraded.
- Surfaces are driver-allocated (`vaCreateSurfaces` with the RT format and decoder/export usage
  hints), exported once with `vaExportSurfaceHandle` (`DRM_PRIME_2`, read-only, composed layers)
  into an `Arc<DmaBufImage>`, and pooled. A surface is free when only the pool holds its `Arc`:
  reference pictures and handed-out `DmaBufFrame`s keep it busy. The pool grows on demand up to
  the stream's reference count + 1 + `OUTPUT_FRAMES_IN_FLIGHT` (8) and then fails with
  `SurfacePoolExhausted`. 8-bit 4:2:0 exports as `NV12`, 10-bit as `P010`.
- Output waits for the picture with `vaSyncSurface` before it is handed out, since wgpu/Vulkan do
  not wait on implicit DMA-BUF fences.

## Test fixtures

- `tests/data/` holds 256x144 clips made by `tests/data/generate.sh` with ffmpeg: 8 frames of
  AV1 in IVF (8-bit and 10-bit 4:2:0, 8-bit 4:4:4), H.264 and HEVC as Annex B with access unit
  delimiters (8-bit 4:2:0, HEVC 10-bit and 4:2:2), plus 12-frame stress clips: H.264 and HEVC
  with B-pyramids, four slices, several references and weighted prediction, H.264 MBAFF with
  B-frames, and AV1 with hidden frames, `show_existing_frame` and 2x2 tiles. `tests/common` has
  the IVF and Annex B splitters.
- `tests/vaapi.rs` checks the public contract on the GPU (one frame per packet, `NV12`/`P010`
  DMA-BUF layout, surfaces of held frames never reused, the 4:2:2 error path, `flush` after
  reordering). The unit tests in `src/vaapi/tests.rs` re-import each exported DMA-BUF as a VA
  surface, read it back with `vaGetImage` and compare a 64-bit FNV-1a of the cropped
  `NV12`/`P010` planes with ffmpeg's decode of the same fixture (`ffmpeg -i <fixture> -f rawvideo
  -pix_fmt nv12|p010le -`, hashed per frame; the stress clips compare the full display-order
  sequence, with the decoder's expected hold count and `flush` completing it). Regenerated
  fixtures need new hashes. Every VAAPI test returns early when the render node, the codec or the
  profile is missing.
- No available encoder writes field pictures (x264 only does MBAFF), so
  `src/vaapi/tests/fields.rs` is a tiny H.264 writer for PAFF streams: 128x64 frames of 16
  macroblocks per field, CAVLC, Main profile. Intra fields are I_PCM with a luma pattern and
  chroma planes unique to a seed. A probe field is a P or B field whose macroblock `i` is
  `P_L0_16x16` (or `B_L0_16x16`/`B_L1_16x16`) with reference index `i % active` and a zero motion
  vector, so one decoded field shows its whole reference list as the seed of each macroblock.
  The tests state the expected lists from the spec (alternating parity, sliding window, MMCO,
  `ref_pic_list_modification`, B ordering, pairing, unpaired fields) and every stream runs with
  one packet per field and one per frame. The marking scenarios were also checked against
  ffmpeg's software decode.
