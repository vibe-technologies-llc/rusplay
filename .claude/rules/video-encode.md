---
paths:
  - "crates/rusplay-encode/**"
---

# Video encode

- `VideoEncoder` is the backend-neutral trait (`Send`, so a session can move it to its encode
  thread). `rusplay_encode::open` picks the backend from `VideoEncoderConfig` and returns
  `BackendNotCompiled` for a backend whose cargo feature is off.
- Backend matrix:

  | Backend | Codecs | Chroma / depth | Input |
  |---|---|---|---|
  | VAAPI | H.264, HEVC, AV1 | 4:2:0; 4:2:2/4:4:4 HEVC and 4:4:4 AV1 where the driver exposes it; 10-bit HEVC/AV1, 12-bit HEVC | DMA-BUF import, zero copy; CPU frames uploaded |
  | NVENC | H.264, HEVC, AV1 | 4:4:4 H.264/HEVC; 4:2:2 H.264/HEVC on GPUs that support it; 10-bit | DMA-BUF through CUDA external memory |
  | x264 | H.264 | 4:2:0, 4:2:2, 4:4:4 (High 4:2:2 / High 4:4:4 Predictive), 8/10-bit | CPU |
  | x265 | HEVC | 4:2:0, 4:2:2, 4:4:4 (RExt), 8/10/12-bit | CPU |
  | SVT-AV1 | AV1 | 4:2:0 (Main profile) | CPU |

  Hardware chroma support is probed at runtime and reported as `ChromaUnsupported`, never
  silently downgraded.
- `rusplay_encode::capabilities(&VideoEncoderConfig)` answers, before any encoder is opened, which
  `(Codec, PixelFormat)` pairs the configured backend can produce, so a session can negotiate the
  stream format with the client. It walks every codec, chroma and depth: VAAPI opens
  `encoder.device` and runs the same profile, entry point and RT format queries as `open`
  (`select` in `src/vaapi/mod.rs`); x265 asks `x265_api_query` for each depth; x264 and SVT-AV1
  answer from the same `supports` predicates their `open` checks. A backend whose feature is off
  is `BackendNotCompiled`. Tests hold `capabilities` and `open` to the same answer for every pair.
- Defaults are low latency: no B-frames, no lookahead, one frame in flight, infinite GOP with
  intra refresh, and IDR only on explicit request (`request_keyframe`, driven by loss reports).
  Bitrate changes are applied in place with `set_bitrate`, without re-creating the encoder.
- The software encoders expose every library parameter: typed config fields for the common ones,
  plus `params` passed verbatim (`x264_param_parse`, `x265_param_parse`, SVT-AV1's
  `svt_av1_enc_parse_parameter`). Errors from parameter parsing name the key
  (`UnknownParameter`, `InvalidParameterValue`).
- Default stream chroma is 4:2:2, to avoid the soft look of 4:2:0 on text and UI edges.

## Software encoders

- Application order of settings: preset and tune, then the low-latency defaults, then the user's
  `params`, then the typed fields. A user param overrides a low-latency default, and a typed field
  (resolution, frame rate, chroma, depth, bitrate, threads, colour description) overrides a param.
  `open` parses every param once, even when the resolution is still unknown, so a bad key fails at
  open and not on the first frame.
- With `video.resolution` unset, the library encoder is opened on the first frame at that frame's
  resolution. A later frame of another size is `FrameResolutionMismatch`.
- Rate control is CBR-like: the typed bitrate is also the VBV max rate, with a buffer of
  `VBV_BUFFER_FRAMES` frames. `set_bitrate` rescales the VBV values the encoder currently runs
  with, so a user override of the buffer keeps its ratio.
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
  `/dev/dri/renderD128`) with `vaGetDisplayDRM`. libva is reached through the bindgen bindings in
  `src/vaapi/ffi.rs`; `src/vaapi/va.rs` wraps display, config, context, buffers, surfaces and images
  in owning types and holds every libva call outside the bitfield setters of the parameter
  structs. Errors are `EncodeError::Va { call, status }`. The VA objects share an `Rc<Display>`, all
  reachable only through the encoder, which is why `VaapiEncoder` is `Send` by an `unsafe impl`.
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
- Colour codes in the VUI and AV1 `color_config` are the H.273 codes from `rusplay-core`
  (`ColorPrimaries::h273_code`, `TransferFunction::h273_code`, the resolved
  `MatrixCoefficients`); custom primaries have no code and fail at open with
  `ColorPrimariesUnsignalable`.
- Rusplay writes the headers and hands them to the driver as packed headers wherever
  `VAConfigAttribEncPackedHeaders` allows: VPS+SPS and PPS for HEVC, SPS and PPS for H.264, the
  slice header of every picture for both, the AV1 sequence header on keyframes and the frame header
  OBU on every frame. radeonsi depends on it: it emits parameter sets only when a packed slice
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
  {-1} and no temporal MVP; AV1 uses 8-bit order hints, refreshes slot 0 from LAST
  (`primary_ref_frame` 0) and one tile unless the size forces more. A temporal delimiter is
  prepended when the driver output lacks one, so every AV1 packet is a temporal unit.
- Rate control: `Cbr` maps to `VA_RC_CBR`, `Vbr` to `VA_RC_VBR` with the maximum as
  `bits_per_second` and the bitrate as `target_percentage`, `ConstantQuality` to `VA_RC_CQP` (AV1
  `base_qindex` is `qp * 5`). The HRD buffer is `VBV_BUFFER_FRAMES` frames at the peak rate
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
  contract below and is packed into a `VAImage` and put into one upload surface.
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
  from exported VA surfaces. Every hardware test returns early when the render node, the codec or
  the format is missing.

## CPU input contract

Software encoders take a `rusplay_capture::Frame` whose buffer is `FrameBuffer::Shm`. A
`FrameBuffer::DmaBuf` is `CpuFrameRequired`. The planar layout is fixed by the configured
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
- `Shm { data, stride }`: `stride` is the luma row pitch in bytes and must be at least
  `width * bytes_per_sample`. The three planes follow each other in `data`, Y then U then V, each
  plane `rows * pitch` bytes. The chroma pitch is `ceil(stride / 2)` where chroma is
  horizontally subsampled (4:2:0, 4:2:2) and `stride` for 4:4:4. Chroma rows are `ceil(height / 2)`
  for 4:2:0 and `height` otherwise. A shorter buffer is `FrameBufferTooShort`, a narrower stride
  `FrameStrideTooSmall`.
- Samples follow `video.color`: range, matrix and transfer are signalled in the stream, never
  applied by the encoder.
- `rusplay_encode::input` holds the contract in code: `fourcc_for`, `pixel_format_of` and
  `YuvFrame::from_frame`, which every software backend and the VAAPI upload use and which a
  colour-convert stage can call to produce frames.
