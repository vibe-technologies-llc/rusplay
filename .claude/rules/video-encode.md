---
paths:
  - "crates/rusplay-encode/**"
---

# Video encode

- `VideoEncoder` is the backend-neutral trait (`Send`, so a session can move it to its encode
  thread). `rusplay_encode::open` picks the backend from `VideoEncoderConfig` and returns
  `BackendNotCompiled` for a backend whose cargo feature is off. `reads_planes` says whether the
  encoder takes `encode_planes(&YuvFrame, timestamp)`, a borrowed view of planar samples with
  their own strides that it reads in place (VAAPI, whose `pack` is the only copy; the host's
  readback uses it so no intermediate `Frame` is filled); the others return `PlanesUnsupported`
  and take a `Frame`.
- Backend matrix:

  | Backend | Codecs | Chroma / depth | Input |
  |---|---|---|---|
  | VAAPI | H.264, HEVC, AV1 | 4:2:0; 4:2:2/4:4:4 HEVC and 4:4:4 AV1 where the driver exposes it; 10-bit HEVC/AV1, 12-bit HEVC | DMA-BUF import, zero copy; CPU frames uploaded |
  | NVENC | H.264, HEVC, AV1 | 4:2:0; 4:2:2 and 4:4:4 H.264/HEVC (8/10-bit) where the GPU reports it (4:2:2 and 10-bit H.264 on Blackwell); 10-bit HEVC/AV1; no 12-bit, no AV1 above 4:2:0 (SDK 13.0) | linear DMA-BUF or opaque Vulkan fd as CUDA external memory, zero copy; CPU frames uploaded |
  | x264 | H.264 | 4:2:0, 4:2:2, 4:4:4 (High 4:2:2 / High 4:4:4 Predictive), 8/10-bit | CPU |
  | x265 | HEVC | 4:2:0, 4:2:2, 4:4:4 (RExt), 8/10/12-bit | CPU |
  | SVT-AV1 | AV1 | 4:2:0 (Main profile) | CPU |

  Hardware chroma support is probed at runtime and reported as `ChromaUnsupported`, never
  silently downgraded.
- `rusplay_encode::capabilities(&VideoEncoderConfig)` answers, before any encoder is opened, which
  `(Codec, PixelFormat)` pairs the configured backend can produce, so a session can negotiate the
  stream format with the client. It walks every codec, chroma and depth: VAAPI opens
  `encoder.device` and runs the same profile, entry point and RT format queries as `open`
  (`select` in `src/vaapi/mod.rs`); NVENC loads the driver, opens a CUDA context and encode session
  on `encoder.gpu_index` and runs the same `Probe` as `open` (`src/nvenc/mod.rs`); x265 asks `x265_api_query` for each depth; x264 and SVT-AV1
  answer from the same `supports` predicates their `open` checks. A backend whose feature is off
  is `BackendNotCompiled`. Tests hold `capabilities` and `open` to the same answer for every pair.
  `Capabilities::format_set` is the same answer as a `FormatSet`, which the host session
  intersects with the client's decodable set (`host.md`).
- Defaults are low latency: no B-frames, no lookahead, one frame in flight, infinite GOP with
  intra refresh, and IDR only on explicit request (`request_keyframe`, driven by loss reports).
  Bitrate changes are applied in place with `set_bitrate`, without re-creating the encoder.
- The software encoders expose every library parameter: typed config fields for the common ones,
  plus `params` passed verbatim (`x264_param_parse`, `x265_param_parse`, SVT-AV1's
  `svt_av1_enc_parse_parameter`). Errors from parameter parsing name the key
  (`UnknownParameter`, `InvalidParameterValue`).
- Default stream chroma is 4:2:2, to avoid the soft look of 4:2:0 on text and UI edges.
- Every backend signals `video.color` in the bitstream: the H.273 codes and range in the VUI or
  AV1 `color_config`, and, when `video.color.hdr` has them, the ST 2086 mastering display colour
  volume and CTA-861.3 content light level as SEI (H.264/HEVC, payload types 137 and 144) or
  metadata OBUs (AV1, types 2 and 1); VAAPI and NVENC H.264 attach them to every keyframe, NVENC
  HEVC/AV1 hand them to the driver's native fields (below). `src/hdr.rs` turns a
  `MasteringDisplay` into `St2086` units (G, B, R, white in 0.00002 steps; luminance in 0.0001
  cd/m²), converts them to AV1 fixed point with `av1_fixed_point` (0.16 chromaticities, luminance
  24.8 and 18.14) and builds the message bodies: `sei_payload` (ST 2086 order, big-endian,
  VAAPI and NVENC H.264), `av1_payload` (R, G, B, white, VAAPI) and `content_light_payload`.
  `src/obu.rs` holds the AV1 temporal delimiter and `AccessUnit`, which both backends use to copy
  the driver's output once into a `Vec` and write the delimiter in front of it when the driver's
  first byte is not one, so the delimiter costs no second copy. That `Vec` comes from the
  session's `FramePool` (`src/pool.rs`) and travels in the `EncodedFrame` as a
  `FrameBytes::recycled`, which hands the `Vec` back to the pool when the frame is dropped
  wherever that happens (the host's send stage), so a steady stream reads back into the same few
  buffers and allocates nothing per frame; the pool keeps at most four idle buffers and a clone
  or `into_vec` detaches from it. Each backend
  has a parameter-mapping test and a test that encodes BT.2020 PQ with Display P3 mastering
  metadata (`hdr::fixtures::hdr10_color`) and decodes it with `rusplay-decode` (VAAPI for
  H.264/HEVC, rav1d and VAAPI for AV1), expecting `DecodedFrame::color` to report all of it.

## Software encoders

- Application order of settings: preset and tune, then the low-latency defaults, then the user's
  `params`, then the typed fields. A user param overrides a low-latency default, and a typed field
  (resolution, frame rate, chroma, depth, bitrate, threads, colour description) overrides a param.
  `open` parses every param once, even when the resolution is still unknown, so a bad key fails at
  open and not on the first frame.
- With `video.resolution` unset, the library encoder is opened on the first frame at that frame's
  resolution. A later frame of another size is `FrameResolutionMismatch`.
- HDR metadata is a typed field: x264 fills `mastering_display` and `content_light_level` in
  `x264_param_t` directly; x265 gets `master-display` (`G(x,y)B(x,y)R(x,y)WP(x,y)L(max,min)` in
  ST 2086 integers) and `max-cll` (`cll,fall`) through `x265_param_parse`; SVT-AV1 gets
  `mastering-display` (the same shape with chromaticities and nits as decimals) and
  `content-light` through `svt_av1_enc_parse_parameter`, which stores the values big-endian in
  `mastering_display`/`content_light_level` (the test reads them with `from_be`).
- Rate control is CBR-like: the typed bitrate is also the VBV max rate, with a buffer of
  `video.vbv_frames` frames (default 2, at least 1, every backend reads it from `VideoConfig`).
  `set_bitrate` rescales the VBV values the encoder currently runs
  with, so a user override of the buffer keeps its ratio. SVT-AV1's low-latency mode takes the
  same frames as milliseconds at the frame rate, floored at 40 ms with the starting level half
  of it (at least 20 ms), so a one-frame buffer at a high frame rate stays at the floor.
- x264 runs CFR with a frame-counter pts. Variable-frame-rate input makes x264 hold one frame
  back to compute its duration, which breaks the one-in-one-out rule. A ledger maps the counter
  back to the capture `Timestamp`.
- x265 reaches the library through `x265_api_query(bit_depth, X265_BUILD)`, which selects the
  8, 10 or 12-bit build at run time; a library without that depth reports `BitDepthUnsupported`
  and a struct-size mismatch with the headers reports `LibraryIncompatible`. The capture
  timestamp in microseconds is the picture pts. Frame threads are pinned to one so each picture
  comes out of the call that submitted it. Only IDR slices count as `keyframe`.
- SVT-AV1 is 4:2:0 only (other chroma is `ChromaUnsupported`) at 8 or 10 bits. `low_latency`
  selects CBR, the low-delay prediction structure (`pred-struct=1`), no lookahead, no scene-cut
  keyframes, IDR-only random access and `rtc`; with it off the library defaults apply and frames
  may come out several calls late. In low-delay mode `svt_av1_enc_get_packet` blocks until the
  frame just sent is out, which is what keeps one frame in flight; outside it the call is polled
  and any queued packets are returned one per `encode`. Forced keyframes use the library's
  `force_key_frames` plus a key picture type on the input. `set_bitrate` attaches a
  `RATE_CHANGE_EVENT` to the next input picture. `threads` maps to `level_of_parallelism`
  (1 to 6, not a thread count). `svt_av1_enc_parse_parameter` does not tell an unknown key from
  a bad value, so both are `InvalidParameterValue` naming the key and value. The library's
  logging is global and finalised by the first `svt_av1_enc_init_handle`, so `svt_av1/log.rs`
  registers `svt_av1_set_log_callback` once (`parking_lot::Once`) before any handle exists. The
  callback formats the message with `vsnprintf` (the bindgen `va_list` pointer is passed straight
  through) and writes `svt-av1 <level> <tag>: <message>` to stderr when the level is at or below
  `log_level` (`off`, `fatal`, `error`, default `warn`, `info`, `debug`); the banner is an info
  message, so the default drops it. The threshold is process-wide and the last encoder opened
  sets it. The library still applies its own `SVT_LOG` filter before the callback runs, so
  `debug` output needs that variable set by the user. `force_key_frames` is set only outside
  `low_latency` (the library warns and clears it there), and dropping an initialised handle sends
  EOS and drains the encoder first, because `svt_av1_enc_deinit` logs an error otherwise.
- Bindings are generated at build time from the system headers (see
  `.claude/rules/dependencies.md`). Each library handle lives in a safe wrapper with a `Drop`
  that closes it, and the only `unsafe` blocks are the FFI calls inside those wrappers.

## VAAPI (`vaapi` feature)

- `VaapiEncoder::new(&VideoConfig, &VaapiConfig)` opens `encoder.device` (default
  `/dev/dri/renderD128`) with `vaGetDisplayDRM`. libva is reached through `rusplay-va` (bindgen
  bindings re-exported as `ffi`, and the owning display, config, context, buffer, surface and image
  wrappers shared with `rusplay-decode`); `src/vaapi/va.rs` keeps only what is encode specific:
  the `EncoderContext` extension (misc and packed-header buffers, the coded buffer), `CodedBuffer`
  reading, `PackedHeader`, and the surface constructors that map `SurfaceFormat` and a DMA-BUF
  import onto `rusplay-va`. The only `unsafe` besides `rusplay-va` is the bitfield setters of the
  parameter structs and the coded-buffer segment walk. Errors are `EncodeError::Va(VaError)`,
  transparent over `rusplay-va`. The VA objects share an `Rc<Display>`, all reachable only through
  the encoder, which is why `VaapiEncoder` is `Send` by an `unsafe impl`.
- Profile choice (`select` in `src/vaapi/mod.rs`): H.264 is 4:2:0 8-bit only, High, then Main,
  then Constrained Baseline; HEVC takes the profile of the format (Main, Main10, Main12,
  Main422_10 for 8- and 10-bit 4:2:2, Main422_12, Main444, Main444_10, Main444_12); AV1 is Profile 0
  for 4:2:0 and Profile 1 for 4:4:4, 8 or 10 bits. `low_power` prefers `VAEntrypointEncSliceLP` and
  falls back to `VAEntrypointEncSlice` (the other way round when off). A candidate counts only when
  the driver lists the profile, the entry point and the RT format; otherwise the open fails with
  `ChromaUnsupported` (chroma above 4:2:0), `BitDepthUnsupported` (more than 8 bits) or
  `CodecUnsupported`. A rate-control mode missing from `VAConfigAttribRateControl` is
  `RateControlUnsupported`; a size outside `vaQuerySurfaceAttributes` and
  `VAConfigAttribMaxPicture*` is `ResolutionUnsupported` (radeonsi on RDNA 4: HEVC from 384x128,
  AV1 from 320x128, H.264 from 96x32).
- Chroma siting reaches the encoders through `open_sited(config, siting)` (`open` is `open_sited` with `ChromaSiting::Left`, for capability probes and tests; the host always calls `open_sited` with `scaling.chroma_siting`). `ChromaSiting::vui_chroma_location` (0 left, 1 centre) goes into the VAAPI H.264/HEVC VUI `chroma_loc_info_present_flag` and both field types, NVENC `chroma_sample_location_*`, x265 `chromaloc` and x264 `i_chroma_loc`; `av1_chroma_position` (vertical for left, unknown for centre, which AV1 cannot express) goes into the VAAPI sequence header, NVENC `chromaSamplePosition` and SVT-AV1 `chroma_sample_position`. x264 only writes the location when it is non-zero, so a left-sited x264 stream leaves it unsignalled. Whatever the bitstream cannot say, `StreamDescription::siting` carries.
- Colour codes in the VUI and AV1 `color_config` are the H.273 codes from `rusplay-core`
  (`ColorPrimaries::h273_code`, `TransferFunction::h273_code`, the resolved
  `MatrixCoefficients`); custom primaries have no code and fail at open with
  `ColorPrimariesUnsignalable`.
- Rusplay writes the headers and hands them to the driver as packed headers wherever
  `VAConfigAttribEncPackedHeaders` allows: VPS+SPS and PPS for HEVC, SPS and PPS for H.264, the
  slice header of every picture for both, the AV1 sequence header on keyframes and the frame header
  OBU on every frame. HDR metadata goes as `VAEncPackedHeaderRawData` (`RAW_DATA` is requested
  with the others) on keyframes, in bitstream order (`packed_buffers` takes `(PackedKind, header)`
  pairs and submits them in the order given): one SEI NAL holding both messages after the PPS and
  before the slice header for H.264/HEVC (`src/vaapi/hdr.rs`), and the metadata OBUs between the
  sequence header and the frame header for AV1, converted to AV1 fixed point (0.16 chromaticities
  in R, G, B order, 24.8 and 18.14 luminance). radeonsi writes raw headers into the output
  verbatim. radeonsi depends on it: it emits parameter sets only when a packed slice
  header is present, takes `nal_unit_type`/`nal_ref_idc` from that slice header, and reads VUI
  (colour description, range, timing) and cropping from the packed SPS. The AV1 frame header has a
  two-byte `obu_size` and fills the `bit_offset_*`/`size_in_bits_*` fields for drivers that patch it
  in place (Intel); radeonsi parses it and writes its own. The AV1 render size is always coded,
  because radeonsi pads the coded frame (to 8x2 on VCN 5, 64x16 on VCN 4) and keeps the captured
  size only as the render size.
- Stream structure: P frames with a single reference, the previous frame, over two reconstructed
  surfaces used alternately; a keyframe on the first frame and on `request_keyframe` only. Intra
  periods in the sequence parameters are `0xffff`, since the picture type of every frame is set by
  Rusplay. H.264 uses POC type 2, a 16-bit `frame_num`, `max_num_reorder_frames` 0 and
  `max_dec_frame_buffering` 1; HEVC uses 64x64 CTBs, 8x8 minimum CBs, a single SPS short-term RPS
  {-1} and no temporal MVP (the VA parameters carry the unwrapped picture order count since the
  last IDR, saturated to `i32`, as libva expects, and only the slice header keeps its 16-bit
  LSB; radeonsi decodes identically across the 65536th frame either way); AV1 uses 8-bit order hints, refreshes slot 0 from LAST
  (`primary_ref_frame` 0) and one tile unless the size forces more. A temporal delimiter is
  prepended when the driver output lacks one, so every AV1 packet is a temporal unit.
- The header writers are tested without a device in `src/vaapi/header_tests/`: every packed header
  (H.264 SPS/PPS/slice, HEVC VPS/SPS/PPS/slice, AV1 sequence and frame header) is parsed back with
  `cros-codecs` across profiles, chroma, depth, resolutions, colour descriptions, frame rates and
  QPs, the VA sequence, picture and slice parameter structs are compared against the parsed
  headers, and the SEI and metadata OBU bytes are checked against hand-derived vectors. A stream
  is driven through `begin`, the header and parameter builders, then `commit`, which `buffers`
  also uses, so the tests walk the same state machine without a VA context.
- Rate control: `Cbr` maps to `VA_RC_CBR`, `Vbr` to `VA_RC_VBR` with the maximum as
  `bits_per_second` and the bitrate as `target_percentage`, `ConstantQuality` to `VA_RC_CQP` (AV1
  `base_qindex` is `qp * 5`). The HRD buffer is `video.vbv_frames` frames at the peak rate
  (`src/rate.rs`, shared with the software encoders) and bit stuffing is off. Rate-control and HRD
  misc buffers go out on every keyframe and on the frame after `set_bitrate`, which is how
  `set_bitrate` applies without re-creating the encoder.
- Intra refresh: when the driver offers rolling rows (preferred) or columns
  (`VAConfigAttribEncIntraRefresh`), every P frame carries `VAEncMiscParameterRIR` stepping across
  the picture in units of 16 (H.264 macroblocks) or 64 (HEVC CTBs, AV1 superblocks), one full sweep
  per `intra-refresh-frames` frames (default: the frame rate rounded up, one second). radeonsi
  repeats the parameter sets when a sweep restarts.
- `params` keys: `intra-refresh-frames` (frames per sweep, `0` turns intra refresh off) and
  `quality-level` (`1` up to the driver's `VAConfigAttribEncQualityRange`, sent on keyframes).
  Another key is `UnknownParameter`, a bad value `InvalidParameterValue`, both at open.
- Input: a `FrameBuffer::DmaBuf` is imported (`DRM_PRIME_2`, one object) and must be in the
  VA-native layout of `video.pixel_format`: `NV12`, `P010`, `P012` (two planes) for 4:2:0,
  `YUYV`, `Y210`, `Y212` for 4:2:2 and `AYUV`, `Y410`, `Y412` for 4:4:4 (one plane). Another fourcc
  is `FrameFormatMismatch`, a wrong plane count `DmaBufPlaneCount`. Imports are cached, keyed by
  the dma-buf's device and inode, modifier, planes and size (16 entries, least recently used out),
  so the buffers PipeWire recycles are imported once. A `FrameBuffer::Shm` follows the CPU input
  contract below and is packed into a `VAImage` and put into one upload surface, as is a
  `YuvFrame` given to `encode_planes`. `format::pack` works per row on slices
  (`chunks_exact` zips the compiler vectorises, a scalar tail for the padding) and is checked
  against the former per-sample implementation, kept in `src/vaapi/format/tests.rs`, over every
  format, odd and padded extents and strides above the row; the ignored `pack_times` test times
  both at 4K. A
  `FrameBuffer::OpaqueFd` is `OpaqueFdUnsupported`. The host's GPU hand-off feeds this path with
  linear `NV12`/`P010`/`P012` exported from RADV (256-byte pitch, chroma after the padded luma);
  radeonsi encodes it bit-identically to the uploaded frame.
- Coded extent: the driver encodes whole blocks and reads the input past the picture, up to
  `device::VAAPI_CODED_ALIGNMENT` (16, also the reconstructed surfaces' alignment). The upload
  image and surface are that padded size (`coded_extent`) and `format::pack` fills the padding by
  repeating the last column and row; a driver-allocated surface's padding is otherwise stale
  memory, which made odd sizes encode nondeterministically. `DeviceInput::extent_alignment`
  carries the same alignment to the host so a handed-off buffer has the same padding (NVENC: 1).
- One frame in flight: `encode` waits for the input surface and reads the coded buffer before it
  returns, so the capture buffer is free again when the call returns. With `video.resolution`
  unset the session opens on the first frame; a later size is `FrameResolutionMismatch`.
  Reconstructed surfaces and the context are 16-aligned (radeonsi derives its padding from the
  context size and the conformance window). The coded buffer holds a raw frame plus 64 KiB;
  more is `CodedBufferOverflow`.
- Tests (`src/vaapi/tests.rs`): headers parse back through the `cros-codecs` parsers without
  hardware; on hardware the output is decoded with `rusplay-decode`'s `VaapiDecoder` (and rav1d for
  AV1, on one thread because of `decode-rav1d-cdef-overlap` in `docs/BUGS.md`), the decoded DMA-BUF is read back with `vaGetImage`, and every frame of a smooth moving
  pattern must reach 34 dB luma PSNR, at 400x216 and at 402x226 for cropping. DMA-BUF input comes
  from exported VA surfaces. Every hardware test skips visibly (`src/hardware.rs`) when the codec or
  the format is missing, and fails under `RUSPLAY_REQUIRE_HW=1` when the render node is missing.

## NVENC (`nvenc` feature)

- Nothing is linked at build time. `src/nvenc/driver.rs` loads `libcuda.so.1` through
  `rusplay-cuda`'s `Driver` (which owns `cuInit`, device lookup and the context push and pop, and
  resolves the symbols NVENC adds on top: `cuCtxCreate_v2`, the external-memory calls and
  `cuMemFree_v2`) and `libnvidia-encode.so.1` through its `open_library` and `symbol`, on every
  `open` and `capabilities` call (a missing library is `CudaError::LibraryNotFound`, a missing
  symbol `CudaError::SymbolMissing`, both inside `EncodeError::Cuda`), refuses a driver whose
  `NvEncodeAPIGetMaxSupportedVersion` is below 13.0 (`DriverTooOld`; driver R570 or newer), fills
  the function list from `NvEncodeAPICreateInstance` and requires every entry it uses
  (`DriverFunctionMissing`). The structs in `src/nvenc/ffi.rs` are hand written for SDK 13.0; their
  sizes, offsets and versions, the `NV_ENC_CAPS` positions and the bit-field positions are tested
  against the header (see `dependencies.md`). `NV_ENC_CAPS` values are positions in the header's
  enum, not documented numbers, so a new SDK means recounting them. Bit fields are `BitField` offsets into the
  `u32` that holds them. Errors are `EncodeError::Cuda(CudaError)`, transparent over
  `rusplay-cuda`, and `EncodeError::Nvenc { call, status }`.
- Ownership: `Driver` (libraries and function pointers) → `CudaContext` (created with
  `cuCtxCreate` on `encoder.gpu_index`, else `CudaError::DeviceMissing`, then popped so no thread keeps
  it current) → `Encoder` (the `nvEncOpenEncodeSessionEx` handle) → input buffers, bitstream
  buffer, registrations and imported CUDA memory, each an `Rc` owner whose `Drop` frees it. Every
  entry point pushes the context for its duration (`CudaContext::enter`) and pops it after, so the
  encoder can move threads; `NvencEncoder` is `Send` by an `unsafe impl`, like VAAPI.
- Selection (`Probe`): the profile is a pure function of codec and format (`config::profile`):
  H.264 High (4:2:0 8-bit), High 10 (4:2:0 10-bit), High 4:2:2 (4:2:2 8/10-bit) and High 4:4:4
  (4:4:4 8/10-bit); HEVC Main, Main10, and FRExt for 4:2:2 and 4:4:4 8/10-bit; AV1 Main for 4:2:0
  8/10-bit. A pair counts when that profile exists, the codec GUID is listed, the NVENC buffer
  format is among `nvEncGetInputFormats`, and the caps agree: `NV_ENC_CAPS_SUPPORT_YUV422_ENCODE`
  for 4:2:2, `_YUV444_ENCODE` for 4:4:4, `_10BIT_ENCODE` for 10-bit, each asked of that codec. No
  GPU generation is assumed: 4:2:2 and 10-bit H.264 appear exactly where the driver reports them. A missing codec is
  `CodecUnsupported`, any other refusal `ChromaUnsupported` (above 4:2:0) or `BitDepthUnsupported`.
  `NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES` gates the rate control (`RateControlUnsupported`) and
  `WIDTH/HEIGHT_MIN/MAX` the resolution (`ResolutionUnsupported`).
- Configuration: `nvEncGetEncodePresetConfigEx` with `preset` (P1 to P7) and `tuning`
  (`ultra-low-latency` by default, `low-latency`, `high-quality`) gives the base `NV_ENC_CONFIG`;
  `config::configure` then forces the low-latency structure over it: infinite GOP and IDR period,
  `frameIntervalP` 1 (no B-frames), no lookahead, zero reorder delay, one reference, parameter sets
  (or the AV1 sequence header) repeated on every IDR, no AUD. The encoder is synchronous
  (`enableEncodeAsync` 0) with picture-type decision on (`enablePTD` 1), so each `encode` returns the
  frame it submitted. Chroma goes in `chromaFormatIDC` (1, 2 or 3), depth in `inputBitDepth` and
  `outputBitDepth`; the colour description fills the H.264/HEVC VUI (signal type and colour
  description present, H.273 codes, full-range flag) or the AV1 `colorPrimaries`,
  `transferCharacteristics`, `matrixCoefficients` and `colorRange`; custom primaries fail at open
  with `ColorPrimariesUnsignalable`.
- Rate control: `Cbr` is `NV_ENC_PARAMS_RC_CBR`, `Vbr` is `_VBR` with the maximum as `maxBitRate`,
  `ConstantQuality` is `_CONSTQP` with the QP on every picture type (AV1 `qp * 5`, at most 255).
  The VBV buffer and initial delay are `video.vbv_frames` frames at the peak rate (`src/rate.rs`).
  `set_bitrate` marks the rate dirty and the next `encode` applies it with `nvEncReconfigureEncoder`
  on the same session, without an IDR.
- Intra refresh is on when the GPU reports `NV_ENC_CAPS_SUPPORT_INTRA_REFRESH`: one sweep over
  `intra-refresh-frames` frames (default the frame rate rounded up, one second) as
  `intraRefreshCnt`, with `intraRefreshPeriod` one frame longer, as the SDK requires the count to
  be smaller.
- `params` keys: `intra-refresh-frames` (`0` turns it off), `spatial-aq` and `temporal-aq`
  (`true`/`false`, `1`/`0`, `on`/`off`; temporal AQ only where the caps report it), `aq-strength`
  (1 to 15), `multipass` (`disabled`, `quarter-resolution`, `full-resolution`) and `split-encode`
  (`auto`, `auto-forced`, `two`, `three`, `four`, `disabled`). Another key is `UnknownParameter`, a bad value
  `InvalidParameterValue`, both at open before the driver loads. Lookahead is not offered, since it
  holds frames back.
- HDR metadata. HEVC and AV1 use SDK 13's native fields: `configure` sets `outputMasteringDisplay`
  and `outputMaxCll` in `NV_ENC_CONFIG_HEVC`/`_AV1` for whichever of the two `video.color.hdr`
  has, and every picture points `pMasteringDisplay`/`pMaxCll` of `NV_ENC_PIC_PARAMS_HEVC`/`_AV1`
  at a `MASTERING_DISPLAY_INFO`/`CONTENT_LIGHT_LEVEL` built once at open (`config::native_hdr`,
  kept in `NvencEncoder`): HEVC in ST 2086 units, AV1 in AV1 fixed point (`St2086::av1_fixed_point`),
  as FFmpeg does; the driver writes the SEI or metadata OBUs. The header gives H.264 no native
  fields, so H.264 keeps the fallback: the SEI payload array of `NV_ENC_PIC_PARAMS_H264` (types 137
  and 144, bodies from `src/hdr.rs`; NVENC wraps them in one SEI NAL) on keyframes
  (`config::sei_payloads`).
- Input. CPU frames follow the CPU input contract and are packed (`layout::pack`) into one
  NVENC-allocated input buffer per session (`nvEncCreateInputBuffer`, locked for the write):
  `NV12` and `YUV420_10BIT` (P010, samples moved to the top 10 bits) for 4:2:0, `NV16` and `P210`
  (interleaved chroma at full height, same MSB alignment) for 4:2:2, `YUV444` and `YUV444_10BIT`
  (three planes, same MSB alignment) for 4:4:4, chroma right after the luma at `pitch * height`.
  DMA-BUF frames must be linear (`DmaBufModifierUnsupported` otherwise) and in `NV12`, `P010`,
  `NV16`, `P210`, `YU24` or `Q410` (`FrameFormatMismatch`), laid out the way NVENC addresses a
  CUDA pointer: one pitch for every plane, a multiple of 4, and plane `i` at
  `offset₀ + i * pitch * height` (`DmaBufPlaneCount`, `DmaBufPlaneStride`, `DmaBufPlaneOffset`,
  `DmaBufStrideUnaligned`, `DmaBufTooShort`; `layout::dma_buf_layout`, unit tested). The fd is
  duplicated and imported with `cuImportExternalMemory` (`OPAQUE_FD`, which takes ownership of the
  fd), mapped whole with `cuExternalMemoryGetMappedBuffer`, and the luma pointer is registered as
  `NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR` with that pitch; each frame maps the registration,
  encodes and unmaps it. Imports are cached like VAAPI's (device and inode, resolution, planes;
  16 entries, least recently used out).
- A `FrameBuffer::OpaqueFd { fd, allocation, size, dedicated, planes }` (the host's GPU hand-off,
  Vulkan memory exported as `OPAQUE_FD`, which is the handle type CUDA documents for Vulkan
  interop) takes the same format and plane checks (`import_request`, unit tested) and is imported
  with `size` as the allocation size (`OpaqueFdTooShort` below the planes) and
  `CUDA_EXTERNAL_MEMORY_DEDICATED` when `dedicated` (`ExternalMemory::handle_desc`, unit tested);
  its cache key is the exporter's `allocation` id, because every opaque fd of a driver may share
  one inode. Not run on NVIDIA hardware (`encode-nvenc-hardware`).
- Output: the bitstream buffer is locked after every picture; `keyframe` is the forced IDR or a
  reported `NV_ENC_PIC_TYPE_IDR`. A keyframe is forced (`FORCEIDR | OUTPUT_SPSPPS`) on the first
  frame and on `request_keyframe`. With `video.resolution` unset the session is initialised on the
  first frame; a later size is `FrameResolutionMismatch`.
- Tests: `ffi.rs` checks every struct size, the offsets the backend writes and every field SDK
  13.0 added, the struct versions, the caps positions, the new buffer formats and profile GUIDs and
  the HDR output bits against the SDK 13.0 header; `config.rs` the profile, preset, tuning,
  rate-control, low-latency, intra refresh, chroma/depth, VUI/AV1 colour, params, H.264 SEI
  fallback and native HDR mapping on zeroed configs;
  `layout.rs` the packing and the DMA-BUF layout checks; `driver.rs` and `tests.rs` the
  missing-library path (`CudaError::LibraryNotFound` from both `open` and `capabilities`, and parameter and
  colour errors before any library loads). The hardware tests in `tests.rs` (capabilities against
  `open`, keyframes on start and request with the bitstream agreeing, every rate-control mode,
  first-frame resolution, AV1 HDR10 through rav1d) skip visibly without the driver (a failure under `RUSPLAY_REQUIRE_HW=1`); none has run
  on NVIDIA hardware yet (`encode-nvenc-hardware` in `docs/FEATURES.md`).

## Device input

- `rusplay_encode::device` says what GPU memory a hardware backend takes, always compiled so the
  host can plan without the backend feature: `device_input(backend, format)` gives the
  `DeviceMemory` (`DmaBuf` for VAAPI, `OpaqueFd` for NVENC), the `DevicePlanes` (`SemiPlanar` for
  4:2:0, `Planar` for NVENC 4:4:4; samples above 8 bits sit in the high bits in both) and the
  fourcc, or `None` (software backends, VAAPI above 4:2:0, whose DMA-BUF formats are packed, and
  NVENC 12-bit). NVENC 4:2:2 is `SemiPlanar` `NV16`/`P210`. `check_device_layout` checks a plane layout against it: the plane
  count for VAAPI, `nvenc::layout::dma_buf_layout` for NVENC (`BackendNotCompiled` without the
  feature). Tests hold both tables to `vaapi::format` and `nvenc::layout`.

## CPU input contract

Software encoders take a `rusplay_core::Frame` whose buffer is `FrameBuffer::Shm`. A
`FrameBuffer::DmaBuf` or `OpaqueFd` is `CpuFrameRequired`. The planar layout is fixed by the configured
`video.pixel_format`, and `format` must match it exactly or the frame is `FrameFormatMismatch`
(never converted):

| Chroma | 8-bit | 10-bit | 12-bit |
|---|---|---|---|
| 4:2:0 | `YU12` | `I010` | `I012` |
| 4:2:2 | `YU16` | `I210` | `I212` |
| 4:4:4 | `YU24` | `I410` | `I412` |

- `YU12`, `YU16` and `YU24` are the DRM fourccs of the 8-bit planar formats. The 10/12-bit codes
  are a Rusplay convention (the kernel has no 3-plane 16-bit DRM formats): planar, little-endian
  16-bit samples, value in the low bits (10-bit maxes at 1023).
- `Shm { data, stride }` (`data` is a `ShmBytes`, a `Vec<u8>` that may return to a pool on drop): `stride` is the luma row pitch in bytes and must be at least
  `width * bytes_per_sample`. The three planes follow each other in `data`, Y then U then V, each
  plane `rows * pitch` bytes. The chroma pitch is `ceil(stride / 2)` where chroma is
  horizontally subsampled (4:2:0, 4:2:2) and `stride` for 4:4:4. Chroma rows are `ceil(height / 2)`
  for 4:2:0 and `height` otherwise. A shorter buffer is `FrameBufferTooShort`. The luma stride
  must be at least `width * bytes_per_sample` and the derived chroma pitch at least
  `chroma_width * bytes_per_sample`, else `FrameStrideTooSmall` (an odd width at minimum stride
  with 10/12-bit 4:2:0 or 4:2:2 trips the chroma check); either pitch not a multiple of the sample
  size is `FrameStrideMisaligned`, since the backends read `u16` samples.
- Samples follow `video.color`: range, matrix and transfer are signalled in the stream, never
  applied by the encoder.
- `rusplay_encode::input` holds the contract in code: `fourcc_for`, `pixel_format_of` and
  `YuvFrame::from_frame`, which every software backend and the VAAPI and NVENC uploads use and which a
  colour-convert stage can call to produce frames.
