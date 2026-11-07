---
paths:
  - "crates/rusplay-decode/**"
---

# Video decode

- `VideoDecoder` is the backend-neutral trait: one `EncodedFrame` (a whole access unit / temporal
  unit; its `data` is a `FrameBytes` that derefs to the payload and may skip a leading prefix of
  its allocation, so decoders read it as `&[u8]`) in, at most one `DecodedFrame` out, plus `flush` for the frames a reordering stream
  still holds. Backends are VAAPI, NVDEC and software. VAAPI and NVDEC each have their own cargo
  feature. The software backend is compiled by `software` (rav1d AV1), `ffmpeg` (libavcodec H.264
  and HEVC), or both; with both off, `open` returns `BackendNotCompiled`.
  `rusplay_decode::open(codec, &VideoDecoderConfig)` dispatches on the config's backend and returns
  `BackendNotCompiled` when that backend is not compiled and `CodecUnsupported` when it cannot
  decode the codec. The backend types (`VaapiDecoder`, `NvdecDecoder`, `Rav1dDecoder`,
  `FfmpegDecoder`) are public too. Decoders are not `Send`: a decoder lives on the thread that
  created it.
- `rusplay_decode::capabilities(&VideoDecoderConfig)` returns `Capabilities` (backend and a
  `FormatSet`), the formats the client announces in its `Hello`. Software: every AV1 chroma and
  depth when `software` is on, and every H.264 and HEVC chroma and depth (4:2:0, 4:2:2 and 4:4:4
  at 8, 10 and 12 bits) when `ffmpeg` is on and `avcodec_find_decoder` finds that codec. VAAPI
  (`src/vaapi/capabilities.rs`) opens the device and keeps a format
  when its profile has a VLD entrypoint and the RT format attribute holds that chroma and depth:
  H.264 High, High10 and High422 (no 4:4:4, no 12-bit), HEVC through the same
  `hevc::profile_for` the decoder uses, AV1 Profile 0 for 8/10-bit 4:2:0, Profile 1 for 8/10-bit
  4:4:4 and Profile 2 for 4:2:2 or 12-bit. NVDEC loads the driver, opens `gpu_index` and keeps a
  format when `cuvidGetDecoderCaps` reports it supported with its output surface in the mask
  (`format::decodes`, unit tested); a missing library or device is the same specific error as
  `NvdecDecoder::new`. A backend that is not compiled is `BackendNotCompiled`.
- `video.decoder.copy_output` (VAAPI only, default false) reads the completed decoder output
  through a libva image and uploads planar CPU frames, bypassing Vulkan DMA-BUF import. It is a
  compatibility path for drivers with corrupt shared-surface output and costs a GPU readback and
  upload per frame. Copy mode advertises only 4:2:0, at every depth the driver decodes, and splits
  NV12/P010/P012 into three planes with high-depth samples shifted to the low bits. The hardware
  pixel test compares copied HEVC 8-bit and 10-bit luma and chroma to native libva readback.
- The decoder backend and its options are `video.decoder` in `rusplay-config`
  (`VideoDecoderConfig`, tagged by `backend`: `vaapi`, `nvdec`, `software`).
- `DecodedFrame` carries the display `resolution`, the `PixelFormat` (chroma and bit depth) the
  stream was coded in, the `color` the bitstream signals (`SignalledColor`, see below), the input
  `timestamp` and a `DecodedBuffer`:
  - `DecodedBuffer::DmaBuf(DmaBufFrame)` from VAAPI, for import into wgpu where
    `rusplay-scale` upscales and converts to the display format. `DmaBufFrame::image()` is a
    `DmaBufImage`: DRM `fourcc`, `modifier`, allocated `width`/`height`, the `objects` (an
    owned fd and size each) and the `planes` (object index, offset, pitch), which is exactly
    what `VK_EXT_image_drm_format_modifier` import needs. The `DmaBufFrame` is a lease: the
    buffer is not reused by the decoder until it is dropped, so it must outlive the GPU work
    that reads it. Importers dup the fd if the API takes ownership.
  - `DecodedBuffer::Cpu(CpuFrame)` from the software decoder (three planes, Y, U, V, borrowed
    from the decoder's own picture without a copy: a rav1d picture for AV1, an `AVFrame` for
    H.264 and HEVC) and from NVDEC (the same three planes in a
    pooled host buffer, see below). Each `CpuPlane` has `data`, `stride` in
    bytes, `width`/`height` in samples and `bytes_per_sample` (1 for 8-bit, 2 for 10/12-bit,
    whose samples are little-endian `u16`s holding the value in the low bits).
- Monochrome streams are rejected with `DecodeError::Monochrome`; there is no 4:0:0
  `ChromaSubsampling`.
- `DecodedFrame::color` reports only what the bitstream says: the VUI (`video_signal_type` for
  the range, `colour_description` for the codes) or the AV1 `color_config` (range always, codes
  when `color_description_present_flag`), and the mastering display and content light level.
  Unsigned fields stay `None` so the client falls back to the stream description. Mastering
  display primaries are mapped back to a named set when they match one exactly; a degenerate or
  out-of-range one is dropped. AV1 metadata fixed point (0.16 chromaticities, 24.8 maximum and
  18.14 minimum luminance) is converted to ST 2086 units in `src/metadata.rs`, shared by every
  backend.
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
- Colour comes from the picture's sequence header (`pri`, `trc`, `mtrx`, `color_range`) and its
  `mastering_display`/`content_light` pointers, which rav1d keeps attached across pictures.
- `max_frame_delay = 1` turns frame threading off so every packet yields its picture on the same
  call; `SoftwareDecoderConfig::threads` sets the tile/postfilter thread count (`None` lets
  rav1d use every logical CPU, at most 256).
- Every packet goes through `Av1Syntax::check_packet` (the AV1 syntax check above) before
  `dav1d_send_data`; it works on a copy of the state and commits only when the whole packet
  passes, drops OBUs outside operating point 0 as rav1d does, and refreshes reference slots when a
  frame's last tile arrives or a key frame is shown again, which is when rav1d does. When rav1d
  still fails a checked packet, the slots are forgotten (and the sequence header too when the
  packet carried one), since rav1d may not have refreshed them. Errors inside the tile data
  (symbol decoder overread, an invalid 4:2:2 partition, an intra block copy vector into the
  current superblock) cannot be seen without decoding; rav1d reports them as a failed send
  (`DecodeError::Rav1dSendData`) and the decoder stays usable (`tests/rav1d.rs` cuts tile data
  short). That relies on the vendored rav1d (`.claude/rules/dependencies.md`).
- rav1d is built with `default-features = false` plus `bitdepth_8` and `bitdepth_16`. Its `asm`
  feature (hand-written SIMD, several times faster) is behind `software-asm`, which implies
  `software` and is forwarded as `rusplay-client/software-asm`. It needs `nasm` at build time on
  x86_64, so `--all-features` builds need it installed; the default build stays free of it.
- `DecodedFrame::resolution` is the AV1 render size from the frame header, clamped per dimension
  to the coded picture, because an encoder that pads the frame (radeonsi codes 402 wide as 408
  and signals 402) must not decode wider than it was captured. The `CpuPlane`s report that
  cropped `width`/`height` over the full coded buffer. A render size larger than the coded one
  (AV1 spatial resize) is not scaled up: the coded picture is returned.
- H.264 and HEVC are the `ffmpeg` feature, below. It does not imply `software`, and `software`
  does not imply it. A build with only `software` returns `CodecUnsupported` for H.264 and HEVC.
  A build with only `ffmpeg` returns `CodecUnsupported` for AV1.

## H.264 and HEVC (`ffmpeg` feature)

- System `libavcodec` and `libavutil`. `crates/rusplay-decode/build.rs` generates the bindings
  from the installed headers. `rusplay-client` and `rusplay-app` forward the feature. It is not a
  default, and it is not a separate `video.decoder` backend: the config stays `software`, and
  `SoftwareDecoderConfig::threads` is the libavcodec thread count (`None` is 0, automatic, at most
  256, the same limit rav1d uses).
- `FfmpegDecoder` opens `avcodec_find_decoder` for H.264 or HEVC. A null decoder is
  `FfmpegDecoderMissing`, and `capabilities` omits that codec, so the advertised set stays what
  `open` accepts. `thread_type` is `FF_THREAD_SLICE` only, set before `avcodec_open2`, so frame
  threading adds no delay. `AV_CODEC_FLAG_LOW_DELAY` is not set. `pkt_timebase` is 1/1000000, so
  capture timestamps round-trip as the picture `pts`. `apply_cropping` stays at its default: the
  picture `width` and `height` are the visible size and the plane pointers are already cropped. A
  non-zero `crop_left` or `crop_top` is `CropOriginUnsupported`. A width or height outside
  `1..=16384` is `ResolutionUnsupported`.
- Each packet is copied into a buffer padded by `AV_INPUT_BUFFER_PADDING_SIZE` zero bytes. The
  `AVPacket` points at that buffer with no `buf`, so `avcodec_send_packet` copies it. An empty
  packet returns `Ok(None)` without a send. `AVERROR(EAGAIN)` drains the pictures already waiting
  and retries the send. `decode` returns the newest picture that send made available.
  `err_recognition` is `AV_EF_EXPLODE`, so a picture whose reference list names a frame the
  decoder does not have fails the packet (`Could not find ref with POC`, then `Error constructing
  the frame RPS`) instead of being skipped. FFmpeg 9 clears every reference flag before that
  failure returns and frees each picture it has already output, so the decoded picture buffer does
  not survive the error and a recovery frame would look for a picture that is gone. The client
  asks for a keyframe and submits nothing else until that keyframe arrives.
  A packet that fails before the reference lists are built (a malformed slice) leaves the picture
  buffer intact, because the decoder is never flushed on an error: `tests/ffmpeg.rs` sends a
  broken slice inside the 8-bit HEVC motion fixture and requires the next frame to match its
  reference checksum, which fails if a send error flushes.
  `flush` sends a null packet and returns every remaining picture in display order, including any
  drained while the flush packet was waiting; a second flush is empty. The picture timestamp is
  `pts`, or the packet timestamp when `pts` is missing. `AV_FRAME_FLAG_CORRUPT` is
  `FfmpegCorruptPicture` and is not presented.
- Accepted layouts are the little-endian planar ones: `YUV420P`, `YUV422P` and `YUV444P`, their
  `P10LE` and `P12LE` forms, and the deprecated `YUVJ420P`, `YUVJ422P` and `YUVJ444P` as the
  matching 8-bit format with full range. 10- and 12-bit samples stay in the low bits. `GRAY*` is
  `Monochrome`. 9-, 14- and 16-bit layouts are `BitDepthUnsupported`. Anything else, including
  semi-planar and hardware formats, is `FfmpegPictureFormat`. A negative `linesize` is rebased to
  the first row with a positive stride. The `AVFrame` is the `CpuFrame` storage and is freed when
  the frame drops.
- Colour is read from the `AVFrame`, not from a second parser. `color_primaries`, `color_trc` and
  `colorspace` go through `with_h273_codes` (a code above 255 is unspecified). `color_range` is
  limited for `MPEG`, full for `JPEG`, and left unset when unspecified. `chroma_location` becomes
  the VUI location `location - 1`; unspecified leaves siting unset, and a location that becomes
  type 6 or more stays unset. Mastering display and content light come from
  `av_frame_get_side_data` (`AV_FRAME_DATA_MASTERING_DISPLAY_METADATA` and
  `AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`). Side data shorter than the struct generated from the same
  headers is ignored; a longer one is read from the front. Primaries are r, g, b rationals,
  converted to ST 2086 units with rounding, and only when `has_primaries` and `has_luminance` are
  set. `MaxCLL` and `MaxFALL` saturate to `u16`.

## VAAPI (`vaapi` feature)

- `VaapiDecoder::new(codec, &VaapiDecoderConfig)` opens `video.decoder.device` (default
  `/dev/dri/renderD128`) with `vaGetDisplayDRM`, and fails with `CodecUnsupported` when the
  driver has no VLD entrypoint for the codec's base profile (H.264 Constrained Baseline/Main/
  High, HEVC Main, AV1 Profile 0).
- libva is reached through `rusplay-va` (bindgen bindings re-exported as `ffi`, and the owning
  display, config, context, buffer, surface and image wrappers shared with `rusplay-encode`);
  `src/vaapi/va.rs` keeps only what is decode specific: `Session` (a VLD config and context pair
  that drops the context first), `DecoderDisplay` (profile, entry point and RT format queries),
  the `SurfacePool` that exports each surface read-only as a `DmaBufImage` (a layer count other
  than one is `ExportLayers`), and the test-only `read_back`. Besides `rusplay-va`, the only libva
  use is the parameter-buffer unions. Errors are `DecodeError::Va(VaError)`, transparent over
  `rusplay-va`.
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
    the field tests double as a check of the driver against the spec: Mesa ignores the slice's
    `RefPicList0/1`, and the firmware also keeps its own marking (it parses
    `dec_ref_pic_marking`) and per-slot picture state across pictures. When its marking disagrees
    with `ReferenceFrames` it conceals the picture by copying reference entry 0, and recreating
    the context drops every earlier reference, so neither re-describing the DPB nor a context
    reset can work around a firmware marking fault (`decode-vaapi-driver-field-long-term`).
  - `hevc.rs`: POC from the previous TemporalId-0 picture, the short- and long-term RPS (from the
    SPS or the slice), RefPicList construction with `list_entry` modification, dependent slice
    segments, RASL skipping after a random-access IRAP, range-extension buffers
    (`VAPictureParameterBufferHEVCExtension`) for the 4:2:2/4:4:4/12-bit profiles. The reorder
    depth is `sps_max_num_reorder_pics` of the highest sub-layer, always present in an SPS;
    `sps_max_latency_increase` is not applied, so a stream relying on it for earlier output waits
    for the reorder depth instead. Pictures with `pic_output_flag` 0 are decoded but not held.
    A picture whose current reference set (`StCurrBefore`, `StCurrAfter`, `LtCurr`) names a
    picture the DPB does not hold is refused with `ReferenceMissing { codec, poc }` before
    anything reaches the driver, after the RPS has been applied: submitting it would predict
    from whatever an invalid entry points at and acknowledge the result as good, so the client
    would never ask for a clean frame. The error takes the client's decode-failure path
    (`DecodeFailed`, a keyframe). A missing `Foll` picture is allowed, as the spec says.
  - `av1.rs`: OBUs from the packet, the eight reference slots, `show_existing_frame`, tile
    groups (a frame is submitted as soon as its last tile arrives, so the parser sees the updated
    reference state for the next header), film grain into a separate display surface. AV1 output
    order is the order frames are shown, so nothing is held: a hidden frame produces no output
    and `show_existing_frame` outputs the referenced slot when it is shown.
  - `reorder.rs`: the codec-independent output queue keyed by picture order count that the H.264
    and HEVC decoders push into. Its depth also sizes the surface pool (`reorder_frames` in
    `StreamFormat`), since held frames keep their surfaces busy.
- AV1 packets are checked before `cros-codecs` sees them, because its OBU reader panics on a
  missing size field, a set reserved bit or a size past the packet: `av1::check_framing` requires
  every OBU to carry a size field that fits the packet, else `Malformed { unit: Obu }`. The parser
  is also primed with a temporal delimiter so it never probes a packet as Annex B. VAAPI and
  NVDEC's colour reader both go through `obu::ObuParser`, which wraps the `cros-codecs` parser.
  An OBU outside operating point 0 comes back as `ObuAction::Drop` with the whole OBU's length
  (`cros-codecs` reports only its header and size field, and the callers would then read the
  payload as the next OBU, whose missing size field trips an assertion). A
  frame header or `show_existing_frame` that is not a key frame while no key frame has refreshed
  all eight reference slots is `Malformed` (an invalid slot has width 0 and the parser divides by
  it). Every OBU then goes through the shared AV1 syntax check (below) before `cros-codecs` parses
  it, fed in lock step: the check's reference slots are refreshed from the `cros-codecs` frame
  header inside `ObuParser::ref_frame_update`, so both see the same reference sizes and order
  hints. A changed sequence header rebuilds the `cros-codecs` parser, and any `cros-codecs` parse
  error rebuilds both from the last accepted sequence header and withdraws the reference state
  until the next key frame. `cros-codecs` reads `temporal_point_info` under the inverted
  `equal_picture_interval` condition, so a sequence with decoder model info is
  `DecoderModelUnsupported` on these paths (it would misparse every shown frame).
- The AV1 syntax check (`src/av1/`, shared by VAAPI, NVDEC's colour reader and the software
  decoder) exists because the third-party parsers trust the tile layout: `cros-codecs` indexes
  64-entry arrays with the tile column and row counters and wraps the tile group size.
  It parses the sequence header and the whole `uncompressed_header` (AV1 spec 5.5 and 5.9) with
  its own bit reader, keeping per reference slot the upscaled width, frame height, order hint,
  key-frame flag and segmentation `ALT_Q` features (all the header syntax depends on), and walks
  the tile groups. Failures are `DecodeError::Av1Syntax(Av1SyntaxError)`: more than 64 tile
  columns or rows, a `context_update_tile_id` past the tile count, a tile group that does not
  start at the next tile or ends past the last, a tile size past the remaining tile group data,
  an empty tile, a tile group or redundant frame header without its frame, a frame header while
  the previous frame still waits for tiles (an identical `OBU_FRAME_HEADER` copy is allowed), a
  frame sized from or showing a slot that holds no known frame, short reference signalling naming
  a non-past slot, superres that does not downscale (rav1d and the spec disagree on whether
  `allow_intrabc` is read then), and a header that runs out of bits. Where the parsers' dialects
  differ the check rejects instead of guessing: an error-resilient frame whose `ref_order_hint`
  disagrees with a slot forgets that slot, a temporal delimiter abandons an unfinished frame, and
  a different sequence header forgets every slot.
- Colour (`src/signal.rs`, shared with NVDEC, as is the Annex B splitter `src/annexb.rs`): the codes and range come from the active SPS VUI or sequence
  header when each picture is decoded, and so does the chroma siting (`SignalledColor::chroma_siting`: VUI `chroma_sample_loc_type_top_field`, AV1 `chroma_sample_position`, rav1d's `chr` in the software decoder); mastering display (SEI 137 / metadata type 2) and content
  light level (SEI 144 / metadata type 1) are read from H.264 SEI NALs, HEVC prefix SEI NALs
  (emulation prevention removed) and AV1 metadata OBUs. `HdrTracker` keeps them per coded video
  sequence: an IDR (H.264), an IRAP (HEVC) or a key frame (AV1) replaces them with what its access
  unit carried, other pictures only update what they carry, so metadata sent on keyframes holds
  for the inter frames between them. The resolved `SignalledColor` lives on the `Stream` and is
  copied into every frame it outputs. A malformed SEI or metadata payload is ignored.
- The stream format (VA profile, chroma, bit depth, coded size) is read from the SPS / sequence
  header; a change rebuilds the `Session` and surface pool. A profile or RT format the driver
  lacks is `ChromaUnsupported` (chroma above 4:2:0), `BitDepthUnsupported` (more than 8 bits) or
  `ProfileUnsupported`, checked with `vaGetConfigAttributes`, never downgraded.
- `annexb::next_nalu` rejects a NAL unit shorter than its header (the HEVC slice and parameter set
  parsers slice past it). `src/hevc.rs` (`hevc::ParameterSets`, one per decoder and per colour
  probe) mirrors the `cros-codecs` HEVC syntax in `src/hevc/` and runs before every `parse_*` call,
  because that parser indexes fixed arrays with counts it reads from hostile headers, in release
  builds too. It keeps what later checks need per id (the SPS fields per SPS id, the PPS fields and
  the SPS it was parsed against per PPS id) and commits them only after `cros-codecs` accepted the
  unit (`keep_*` after `parse_*`), so both hold the same sets. It rejects with `Malformed`: a
  sub-layer count of 7 (the profile tier level arrays hold 6), a scaling list predicted from a
  matrix id that does not precede it, a short-term reference picture set predicted from one of 64
  or more pictures, a palette predictor initializer count past the 128 slots, more than 19 tile
  columns or 21 tile rows (the arrays are one short of the spec's 20 and 22), explicit tile column
  widths or row heights that do not fit the picture in CTBs, a slice header with more than 16
  long-term pictures or more than 32 entry points, and a slice of a PPS the checks never accepted.
  The slice scan (`src/hevc/slice.rs`) follows 7.3.6.1 up to the entry point count, including
  short-term set prediction, long-term pictures, reference list modification and the prediction
  weight tables, because the entry point count depends on all of them. The scans copy the parser's
  layout, not the spec's, wherever the two differ (a sub-layer's profile and level are both read
  only when its level flag is set; the chroma weight flags of list 1 follow `chroma_format_idc`,
  list 0's follow `chroma_array_type`), since a scan that desyncs from the parser misses what it
  looks for. A unit the scan cannot finish (truncated, or a value the parser rejects) passes the
  scan with nothing kept, and the parser reports it. Release builds are `panic = "abort"`, so
  `catch_unwind` around `cros-codecs` is not available and every parse panic has to be
  pre-validated or recorded in `docs/BUGS.md`.
- Hostile headers are rejected before `cros-codecs` computes on them. An H.264 SPS above level
  6.2's 139264 macroblocks, or 1055 macroblocks in either dimension, is `ResolutionUnsupported`
  (the DPB size divides by the macroblock count). Picture order count arithmetic on SPS and slice
  header values wraps (`wrapping_*`), so a crafted stream cannot panic a debug build.
- The display size is the coded size minus the right and bottom crop (H.264 frame cropping, HEVC
  conformance window, computed in `src/vaapi/crop.rs` with checked arithmetic), and
  `Stream::output_sized` clamps every report to the coded size, as the software path does for the
  AV1 render size. A crop past the coded size is `Malformed`, and a left or top crop is
  `CropOriginUnsupported` because a frame carries a size, not an origin.
- Surfaces are driver-allocated (`vaCreateSurfaces` with the RT format and decoder/export usage
  hints), exported once with `vaExportSurfaceHandle` (`DRM_PRIME_2`, read-only, composed layers)
  into an `Arc<DmaBufImage>`, and pooled. A surface is free when only the pool holds its `Arc`:
  reference pictures and handed-out `DmaBufFrame`s keep it busy. The pool grows on demand up to
  the stream's reference count + 1 + `OUTPUT_FRAMES_IN_FLIGHT` (8) and then fails with
  `SurfacePoolExhausted`. 8-bit 4:2:0 exports as `NV12`, 10-bit as `P010`.
- Output waits for the picture with `vaSyncSurface` before it is handed out, since wgpu/Vulkan do
  not wait on implicit DMA-BUF fences.

## NVDEC (`nvdec` feature)

- Nothing NVIDIA is needed to build: `src/nvdec/driver.rs` loads `libcuda.so.1` through
  `rusplay-cuda`'s `Driver` (which owns `cuInit`, device lookup and the context push and pop, and
  resolves the symbols NVDEC adds on top: the primary-context calls and `cuMemcpy2D_v2`) and
  `libnvcuvid.so.1` through its `open_library` and `symbol` when
  `NvdecDecoder::new(codec, &NvdecDecoderConfig)` runs,
  so `--all-features` builds and tests on machines without the driver. `src/nvdec/ffi.rs` holds
  hand-written `#[repr(C)]` declarations of exactly the CUDA driver and cuviddec/nvcuvid subset in
  use, taken from the SDK 13.1 headers (FFmpeg's `nv-codec-headers`); its tests pin every struct
  size and the offsets that matter on LP64. `CUVIDPICPARAMS` is opaque, since it only travels from
  the parser to `cuvidDecodePicture`. A missing library is `CudaError::LibraryNotFound`, a missing symbol
  `CudaError::SymbolMissing`, a failed driver call `CudaError::Call`, a `gpu_index` past the
  device count `CudaError::DeviceMissing`, all inside `DecodeError::Cuda`; a failed `cuvid` call is
  `DecodeError::Cuvid { call, status }`.
- `new` retains the device's primary context (pushed around every call and released on drop) and
  fails with `CodecUnsupported` when `cuvidGetDecoderCaps` reports no 8-bit 4:2:0 decoder for the
  codec. Decoding uses the driver's parser (`cuvidCreateVideoParser`) with display delay 0, a
  1 MHz clock carrying `Timestamp` micros through untouched, `ulErrorThreshold` 0, and every
  packet sent with `CUVID_PKT_ENDOFPICTURE` so the picture is decoded on the same call instead of
  when the next start code arrives. The parser does reordering itself; `flush` sends
  `CUVID_PKT_ENDOFSTREAM` and returns every picture it still held, oldest first.
- Parser callbacks run synchronously inside `cuvidParseVideoData` on a heap `ParserState` the
  parser points at; they never panic, and a failure is stored there, returned as the callback's
  0 and handed back by `decode`. The sequence callback maps `CUVIDEOFORMAT` to a `StreamFormat`
  (`src/nvdec/format.rs`, pure and unit tested): chroma from `chroma_format` (monochrome is
  `Monochrome`), the larger of the luma and chroma depths, the display area clamped to the coded
  size, and an output surface per format, `NV12`/`P016` (4:2:0), `NV16`/`P216` (4:2:2),
  `YUV444`/`YUV444_16Bit` (4:4:4). It checks `cuvidGetDecoderCaps` for that chroma and depth
  (`ChromaUnsupported`, else `BitDepthUnsupported`, else `CodecUnsupported`, the same order
  VAAPI uses), the output format mask and the size limits (`ResolutionUnsupported`), creates the
  decoder (`PreferCUVID`, weave, `min_num_decode_surfaces`, one mapped output surface) and
  returns the surface count to the parser. A changed format downloads pictures still pending
  from the old decoder first, then recreates it.
- The decoder crops to the display area and its target is the cropped size rounded up to even
  (NVDEC requires it), never a different size, so the driver never scales; `DecodedFrame` reports
  the exact display size and the planes are cut to it.
- Display callbacks only queue `CUVIDPARSERDISPINFO`. After the packet, the newest queued picture
  is mapped (`cuvidMapVideoFrame64`), copied plane by plane to host memory with `cuMemcpy2D` and
  unmapped at once, so no decoder surface is leased to the client; older ones from the same packet
  are dropped (newest wins). Semi-planar chroma goes through a staging buffer and is split into U
  and V, and 16-bit surfaces, which hold the value in the high bits, are shifted down so the
  `CpuPlane`s follow the low-bit contract (`src/nvdec/host.rs`). The planar buffer comes from a
  pool that frames return to on drop. The client presents it like a software frame; zero-copy
  hand-off is `decode-nvdec-zero-copy` in `docs/PERFORMANCE.md`.
- Colour does not come from `CUVIDEOFORMAT::video_signal_description`, which cannot say whether
  the bitstream signalled anything. `src/nvdec/color.rs` runs the same `cros-codecs` header
  parsing and `HdrTracker` as VAAPI over each packet before it goes to the parser: the SPS of
  the first slice's PPS (H.264, HEVC first slice segment) or the sequence header (AV1, with
  `ref_frame_update` after each frame header so later headers parse), with IDR, IRAP or key frame
  as the random access point. Every output frame carries the colour resolved for the latest
  packet, as VAAPI's `Stream::color` does. A header `cros-codecs` cannot parse fails `decode` with
  `Malformed`, as on VAAPI.
- Unit tests need no NVIDIA hardware: layouts, the format and capability mapping, plane layout,
  deinterleaving and shifting, buffer recycling, colour from the fixtures and an injected HEVC
  mastering SEI, and the missing-library error. Everything that talks to the driver is unverified
  until `decode-nvdec-hardware` in `docs/FEATURES.md`.

## Fuzzing

- The `fuzzing` feature (never in a shipped build; `--all-features` turns it on) adds
  `VaapiDecoder::detached(codec)` and `fuzzing::ColorProbe`. A detached `Display` has a null
  `VADisplay`: `Display::is_detached` (constant `false` without the feature) makes every libva call
  in `rusplay-va` (its `detached` feature, which `fuzzing` turns on) a no-op and the surface pool hands out `PoolSurface::detached`, so the whole H.264,
  HEVC and AV1 decode path (parsing, DPB, reference lists, parameter buffers, reorder queue) runs
  without a device. `ColorProbe` is NVDEC's colour reader, which parses the same bitstreams.
- `fuzz/` has `decode_h264`, `decode_hevc`, `decode_av1` (the detached decoder), `decode_color`
  and `decode_software` (`Rav1dDecoder`). The input is a sequence of packets, each a big-endian
  `u16` length and its bytes (`rusplay_fuzz::packets`); `decode_color` takes a codec selector byte
  first. Run them with `cargo +nightly fuzz run -O <target> -- -max_len=65536`; `-O` drops
  debug assertions and overflow checks to match release builds. `cros-codecs` builds without
  overflow checks in every profile (`[profile.dev.package.cros-codecs]` in the workspace manifest,
  `[profile.release.package.cros-codecs]` in `fuzz/Cargo.toml`), because its parsers overflow on
  hostile headers where release wraps; the fuzz override also turns debug assertions on, so that
  cargo passes an explicit `-C overflow-checks=off` that outranks cargo-fuzz's
  `-Cdebug-assertions`. Seed a corpus from `tests/data` by splitting the
  fixtures into packets. Open findings are in `docs/BUGS.md` (`decode-cros-codecs-*`).

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
  fixtures need new hashes. Every VAAPI test skips visibly (`src/hardware.rs`) when the codec or
  the profile is missing, and fails under `RUSPLAY_REQUIRE_HW=1` when the render node is missing; a
  `VaapiDecoder::new` error other than `CodecUnsupported` fails the test.
- The reference logic has device-free unit tests next to it: `src/vaapi/h264/dpb/tests.rs` (marking
  by every MMCO, sliding window, initial and modified lists for frames and fields, the three
  order count types), `src/vaapi/hevc/tests.rs` (order count unwrapping, the RPS helpers,
  reference lists, profile mapping) and `src/vaapi/av1/tests.rs` (stream format, CDEF packing). The bitstream guards have their own: `src/hevc/tests.rs`, `src/obu/tests.rs`, `src/annexb/tests.rs`,
  and `src/av1/tests.rs`, whose `BitWriter`, sequence header and key frame builders craft headers
  (65 tile columns or rows, oversized and empty tiles, out-of-order tile groups, empty reference
  slots) that `src/obu/tests.rs` also pushes through `ObuParser` to show they never reach
  `cros-codecs`.
  They build `PoolSurface::detached` surfaces, which carry an id and an empty image.
- No available encoder writes field pictures (x264 only does MBAFF), so
  `src/vaapi/tests/fields.rs` is a tiny H.264 writer for PAFF streams: 128x64 frames of 16
  macroblocks per field, CAVLC, Main profile. Intra fields are I_PCM with a luma pattern and
  chroma planes unique to a seed. A probe field is a P or B field whose macroblock `i` is
  `P_L0_16x16` (or `B_L0_16x16`/`B_L1_16x16`) with reference index `i % active` and a zero motion
  vector, so one decoded field shows its whole reference list as the seed of each macroblock.
  The tests state the expected lists from the spec (alternating parity, sliding window, MMCO,
  `ref_pic_list_modification`, B ordering, pairing, unpaired fields) and every stream runs with
  one packet per field and one per frame. The marking scenarios were also checked against
  ffmpeg's software decode. A scenario the driver gets wrong stays as an `#[ignore]`d test named
  in its `docs/BUGS.md` item, run with `cargo test -p rusplay-decode --features vaapi --lib --
  --ignored` to check a driver update.

- `tests/vaapi_import.rs` compares both HEVC luma and chroma planes read through Vulkan with
  libva image readback for 8-bit and 10-bit fixtures, repeating the sequence to reuse decoded
  surfaces. It submits the same external ownership handoff as the client and checks every byte.
- `tests/vaapi_motion.rs` replays production-encoded HEVC motion and reference-recovery
  fixtures at 1280x720. Rusplay CPU-copy decode and the installed FFmpeg VAAPI decode each
  compare all three planes to FFmpeg software reference checksums. It needs no encoder,
  network or Vulkan device, so it can isolate client hardware from host and presentation
  behaviour. Fixture provenance, regeneration and the client command are in
  `crates/rusplay-decode/tests/data/HEVC-MOTION.md`.
- `tests/vaapi_pipeline.rs` (`#[ignore]`d, minutes long) runs the motion fixtures with decode and
  Vulkan readback on separate threads and frames in flight, against the same software checksums.
  It compares the client's path with waiting for the dma-buf write fence and with holding surfaces
  out of reuse, and prints the device, adapter and surface layout, to separate causes on a client
  whose DMA-BUF output is corrupt while `copy_output` is clean.

HEVC retains every picture named by the RPS, including the oldest picture when the stored DPB reaches sixteen pictures. The fifteen VA reference slots describe references before the current picture is inserted. Detached VAAPI output returns the detached surface image.
