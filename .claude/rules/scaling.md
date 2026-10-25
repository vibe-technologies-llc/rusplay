---
paths:
  - "crates/rusplay-scale/**"
  - "crates/rusplay-config/src/scaling.rs"
---

# Scaling

- All resampling runs on the GPU through wgpu compute shaders (`resample.wgsl`). The CPU only
  builds the tap tables (`TapTable`) from the kernel functions in `kernel.rs`, once per
  resolution or filter change, and uploads them. Hardware scalers (VAAPI VPP, NVENC's internal resize,
  compositor scaling) are never used, because they are bilinear-grade.
- Host downscale (capture resolution above stream resolution) defaults to **Hermite** in linear
  light: a non-ringing weighted average with the kernel stretched by the scale ratio, so every
  source pixel contributes.
- Client upscale defaults to **Lanczos3**, with sigmoidal upscaling and optional anti-ringing.
  Chroma (4:2:0 and 4:2:2 planes) has its own filter for both directions.
- Filters are a closed enum, `ScaleFilter` in `rusplay-core`. Separable filters: bilinear,
  Hermite, Catmull-Rom, Mitchell, Lanczos 2/3/4. Polar (EWA, jinc based): EWA Lanczos, EWA
  Lanczos Sharp, Ginseng, Hann, Robidoux, Robidoux Sharp. Kernel definitions follow mpv/libplacebo,
  so results can be compared against them.
- Separable filters run as two passes (horizontal, then vertical) through an `rgba16float`
  intermediate. Polar filters need a single 2D pass over a circular footprint. That shader is
  still to come (see `docs/FEATURES.md`).
- Linear light comes from sampling an sRGB view of the source, so `textureLoad` returns
  linearised values. Do not add manual transfer functions where a texture view format does the
  job.
- Anti-ringing follows mpv/libplacebo: each separable pass tracks the min/max of the two source
  texels bracketing the sample centre (`TapTable::anchors` holds the left one,
  `floor(centre)`), and the result is `mix(sum, clamp(sum, low, high), strength)`. The strength
  is `ScaleSettings::antiringing` (0..=1), which the client fills from
  `scaling.upscale.antiringing`; the host's downscale passes 0.
- `ScalePipelines` holds the compiled pipelines and explicit bind group layouts, built once per
  device and cloned (cheap handles) into every `Scaler`. A `Scaler` is one cached plan for a
  `ScaleSettings`: tap table buffers, params uniforms, the intermediate texture and every bind
  group that does not involve a frame texture are built in `Scaler::new`, and
  `Scaler::configure` rebuilds the tables only when the filter or a resolution changes
  (`Reconfigured::Tables`); a change to the shaping parameters (anti-ringing, sigmoid) only
  replaces the params uniforms and their bind groups (`Reconfigured::Parameters`). Nothing
  is allocated on the per-frame path except a bind group for a frame view not seen recently.
- Bind groups are split by lifetime: group 0 is the source texture, group 1 the storage
  destination, group 2 the pass's tables and params. Frame views get groups 0 and 1 from a small
  per-scaler cache keyed by `TextureView` (`CACHED_VIEW_GROUPS`), sized for a capture or decode
  swapchain cycling a few buffers. Sources are bound as non-filterable float textures, so any
  float format can be the input; destinations are `rgba16float`.
- WGSL follows the style rules too: no comments. Naga rejects WGSL reserved words as names
  (`target` is one), so a shader that only fails at pipeline creation is caught by the GPU tests.
- GPU tests run headless on the first high-performance adapter (`testing::gpu`, shared through a
  `OnceLock`) and return early when no adapter exists, so GPU-less machines pass. Each pass is
  compared with a CPU reference in `reference.rs` built from the same `TapTable`s and kernels,
  with clamp-to-edge addressing and `f16` rounding of every stored texel, to within a few
  `rgba16float` ulps (`4/1024`). Test images are quantised to `f16` before upload so the
  reference and the GPU see identical inputs.
