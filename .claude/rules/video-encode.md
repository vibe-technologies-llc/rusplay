---
paths:
  - "crates/rusplay-encode/**"
---

# Video encode

- `VideoEncoder` is the backend-neutral trait. `rusplay_encode::open` picks the backend from
  `VideoEncoderConfig` and returns `BackendNotCompiled` for a backend whose cargo feature is off.
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
  `svt_av1_enc_parse_parameter`). Errors from parameter parsing name the key.
- Default stream chroma is 4:2:2, to avoid the soft look of 4:2:0 on text and UI edges.
