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
  | VAAPI | H.264, HEVC, AV1 | 4:2:0; 4:2:2/4:4:4 HEVC where the driver exposes it; 10-bit HEVC/AV1 | DMA-BUF import, zero copy |
  | NVENC | H.264, HEVC, AV1 | 4:4:4 H.264/HEVC; 4:2:2 H.264/HEVC on GPUs that support it; 10-bit | DMA-BUF through CUDA external memory |
  | x264 | H.264 | 4:2:0, 4:2:2, 4:4:4 (High 4:2:2 / High 4:4:4 Predictive), 8/10-bit | CPU |
  | x265 | HEVC | 4:2:0, 4:2:2, 4:4:4 (RExt), 8/10/12-bit | CPU |
  | SVT-AV1 | AV1 | 4:2:0 (Main profile) | CPU |

  Hardware chroma support is probed at runtime and reported as `ChromaUnsupported`, never
  silently downgraded.
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
- Bindings are generated at build time from the system headers (see
  `.claude/rules/dependencies.md`). Each library handle lives in a safe wrapper with a `Drop`
  that closes it, and the only `unsafe` blocks are the FFI calls inside those wrappers.

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
  `YuvFrame::from_frame`, which every software backend uses and which a colour-convert stage can
  call to produce frames.
