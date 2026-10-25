---
paths:
  - "crates/rusplay-scale/**"
  - "crates/rusplay-config/src/scaling.rs"
---

# Scaling

- All resampling runs on the GPU through wgpu compute shaders (`resample.wgsl`). The CPU only
  builds the tap tables (`TapTable`) from the kernel functions in `kernel.rs`, once per
  resolution or filter change, and uploads them. Hardware scalers (VAAPI VPP, NVENC's internal
  resize, compositor scaling) are never used, because they are bilinear-grade.
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
  intermediate. Polar filters run as a single 2D pass (`polar.wgsl`): every source texel whose
  distance from the sample centre, divided per axis by the stretch `max(ratio, 1)`, is below
  the kernel radius contributes, weighted by the radial kernel and normalised by the weight
  sum. The footprint is a circle of the kernel radius when upscaling, widened by the downscale
  ratio when downscaling (an ellipse if the two axes scale differently). Weights come from a
  1D LUT of the radial kernel (`RADIAL_LUT_SIZE` entries over `[0, radius]`, linearly
  interpolated) uploaded as a storage buffer once per plan. Anti-ringing for polar uses the 2x2
  texels around the centre, and the pass both sigmoidizes its input and unsigmoidizes its
  output. `Scaler` picks the plan from `Kernel::geometry`.
- Linear light comes from sampling an sRGB view of the source, so `textureLoad` returns
  linearised values. Do not add manual transfer functions where a texture view format does the
  job. The one manual transfer is the OETF in colour conversion: storage writes cannot target an
  sRGB format, BT.709 has no view format, and the output is a buffer anyway.
- Anti-ringing follows mpv/libplacebo: each separable pass tracks the min/max of the two source
  texels bracketing the sample centre (`TapTable::anchors` holds the left one,
  `floor(centre)`), and the result is `mix(sum, clamp(sum, low, high), strength)`. The strength
  is `ScaleSettings::antiringing` (0..=1), which the client fills from
  `scaling.upscale.antiringing`; the host's downscale passes 0.
- Sigmoidal upscaling is libplacebo's sigmoid: `ScaleSettings::sigmoid` (`Sigmoid { center,
  slope }`, defaults 0.75 and 6.5, from `scaling.upscale.sigmoid_center`/`sigmoid_slope` when
  `scaling.upscale.sigmoid` is on) maps linear light into sigmoid space before filtering and back
  afterwards. It is fused into the passes rather than run as its own: the horizontal pass
  sigmoidizes every texel it loads, the vertical pass unsigmoidizes what it stores, so the
  `rgba16float` intermediate holds sigmoid-space values. Offset and scale are computed on the
  CPU and sit in the params uniform. The host never sets it, since libplacebo applies it to
  upscaling only.
- `shaping.wgsl` is the shared prelude (the `Shaping` uniform block, `shape_input`,
  `shape_output`, `antiring`) concatenated in front of each resampling shader at pipeline
  creation; `shaping.rs` writes the matching words and holds the CPU reference versions.
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
  per-scaler `GroupCache` keyed by `TextureView` (`CACHED_FRAME_GROUPS` entries), sized for a
  capture or decode swapchain cycling a few buffers. Sources are bound as non-filterable float textures, so any
  float format can be the input; destinations are `rgba16float`.
- RGB to Y'CbCr is `ColorConverter` (`convert.rs`, `convert_*.wgsl`), the host's last GPU step
  before the encoder. Input is any float texture; with `InputLight::Linear` (the scaler's
  linear-light output) the `TransferFunction` OETF is applied first, with `InputLight::Encoded`
  the values are used as they are. sRGB and BT.709 treat linear 1.0 as full scale. PQ
  (SMPTE ST 2084) and HLG (ARIB STD-B67 / BT.2100) treat the input as display light in which
  1.0 is `HdrLevels::reference_white_nits` (default 203, BT.2408), from
  `video.color.hdr.reference_white_nits`, and values above full scale are meaningful. PQ
  scales by white / 10000 nits and clamps to 0..=1. HLG scales by white / `peak_nits`
  (`video.color.hdr.peak_nits`, default 1000), applies the BT.2100 inverse OOTF with
  `gamma = 1.2 + 0.42 * log10(peak / 1000)` using the matrix's luma row as the luminance, and
  then the HLG OETF. Both levels are validated into `1..=10000` (`ReferenceWhiteOutOfRange`,
  `PeakLuminanceOutOfRange`) and sit in the params uniform's `hdr` vector. The other
  transfers ignore them.
  Gamma 2.2 encodes as `x^(1/2.2)` and `Linear` passes values through clamped. The matrix is
  `rusplay_core::MatrixCoefficients` (BT.709, BT.601 or BT.2020 non-constant luminance, the
  caller resolving it with `ColorSpace::matrix_coefficients`), range and depth follow H.273: full range codes are `(2^n - 1) * E` and
  `2^(n-1) + (2^n - 1) * C`, limited are `219/224 * 2^(n-8)` around `16/128 * 2^(n-8)`.
- Every source pixel is loaded and run through the OETF exactly once, in the first pass, which
  computes Y'CbCr there and packs luma straight into the output buffer. Chroma is then
  downscaled in gamma space (as every encoder expects) with `scaling.downscale.chroma_filter`
  through `TapTable`s at an exact 2:1 ratio, so odd sizes round the chroma plane up and the
  edge clamps. Horizontal siting is `ChromaSiting::Left` by default (MPEG-2/H.264/HEVC type 0,
  co-sited with even luma columns, a phase of -0.5 source pixels) or `Center`; vertical 4:2:0
  siting is always centred. The chroma plane is built by one of three paths.
  4:4:4 is one dispatch (`convert_direct`): each invocation owns one output word of luma and
  writes the matching words of both chroma planes, with no intermediate, and the chroma filter
  is not used. Separable filters on 4:2:0 and 4:2:2 take two (`convert_rows`, `pack_chroma`):
  `convert_rows` has each workgroup encode its row segment plus the filter's halo into
  workgroup memory (`TILE_STRIDE` entries per row, so a filter may have at most
  `MAX_CHROMA_TAPS` horizontal taps, checked by a test), packs luma and writes the horizontally
  filtered CbCr into an `rg32float` intermediate, and `pack_chroma` applies the vertical taps
  and packs. 4:4:4 and the 4:2:2 vertical direction use `TapTable::identity`,
  never a kernel at 1:1 (Mitchell would blur).
- Polar (EWA) chroma filters take three passes on 4:2:0: `convert_full` packs luma and writes the
  full-resolution encoded CbCr into an `rg32float` texture, the polar pass (`polar.wgsl`
  assembled with `destination_rg32float.wgsl`, run by `PolarPlan::chroma`) resamples it to the
  chroma plane at an exact 2:1 ratio in both axes with no shaping, and `pack_chroma` quantises
  with identity vertical taps. Siting enters as `PolarGeometry::phase`, a shift of the sample
  centre in source pixels (-0.5 horizontally for `Left`). 4:2:2 rejects polar filters with
  `PolarChromaFilterUnsupported`: its vertical axis is 1:1 and a 2D kernel there would blur it,
  which the identity taps exist to avoid. EWA Lanczos chroma costs about 1.7 ms per 4K frame on an
  RX 9070 against 0.3 ms for Hermite, so it is an opt-in quality choice.
- The output is one storage buffer per `YuvFrame`, described by `FrameLayout`: planes
  `Luma, Cb, Cr` (`PlaneLayout::Planar`, I420/I422/I444 and their `p10le`/`p12le` forms) or
  `Luma, CbCr` (`SemiPlanar`, NV12/NV16/NV24 and P010/P210/P410 style). Samples are one byte
  at 8 bits and two little-endian bytes above, `SampleAlignment::High` (MSB, the P010 family)
  for semi-planar and `Low` (LSB) for planar. Every row stride and so every plane offset is a
  multiple of `row_alignment` (a power of two of at least 4, `DEFAULT_ROW_ALIGNMENT` is
  wgpu's 256 so a plane can be copied straight into a texture). Shaders write whole `u32`
  words, one invocation per word, so no atomics are needed and row padding is zero.
  `ColorConverter::encode` refuses a frame created for another layout (`FrameLayoutMismatch`).
- Y'CbCr to RGB on the client is `RgbConverter` (`unpack.rs`, `unpack_*.wgsl`), reading a
  `YuvFrame` upload buffer laid out by `FrameLayout::packed` (planar or semi-planar, any depth).
  `unpack_chroma` applies the horizontal chroma taps straight from the buffer into an `rg32float`
  intermediate at luma width, `unpack_rgb` applies the vertical taps, the `MatrixCoefficients::inverse_rows`
  and, for `OutputLight::Linear`, the inverse OETF (sRGB, BT.709, gamma 2.2 or linear; PQ and HLG
  must use `OutputLight::Encoded`), into an `rgba16float` storage texture. Chroma is upsampled with
  `scaling.upscale.chroma_filter` at exactly 1:2 (left siting is a phase of +0.25 chroma samples)
  and must be separable. `reference::unpack_rgb` is its CPU reference.
- Workgroup shapes are per pipeline (`ShaderPass` carries the pipeline and its shape, the
  shaders take it through the `WORKGROUP_X`/`WORKGROUP_Y` overrides in `workgroup.wgsl`, and
  `gpu::dispatch` divides by the same shape). Measured on an RX 9070 XT: polar runs at 64x1
  (17% faster than 8x8 for EWA Lanczos 1080p to 4K, 14% for EWA Robidoux 4K to 1080p); the
  separable passes and the conversion's `convert_direct` and `pack_chroma` stay at 8x8, where
  every other shape was equal or slower; `convert_rows` is 64x1 because its workgroup memory
  tile is one row (32, 128 and 32x2 measured within noise of it).
  Shared-memory tiles were tried and rejected: a tiled polar pass was 35% slower at any tile
  size and shape (the inner loop is ALU-bound and the texture cache already captures the
  reuse), and the separable passes already run at roughly 85% of memory bandwidth even for an
  8K to 720p Hermite downscale, so a tile has nothing left to win.
- Tone and gamut mapping (`color.rs`, `color/`, `color_*.wgsl`) share `ScalePipelines`
  (`ColorPipelines`: the map pass at 8x8 with a params and tone-state layout, and the
  histogram and peak passes on an analysis layout), the source and destination layouts and
  the `GroupCache` scheme with the scaler. Their design is in `.claude/rules/color.md`.
- `bench.rs` is an ignored test that prints per-frame GPU times for the representative
  workloads: `cargo test --release -p rusplay-scale scaling_frame_times -- --ignored
  --nocapture`. It takes the best of several batches after a long warm-up, because the GPU's
  clocks otherwise swing results by 30%. Rerun it before and after any shader performance
  change.
- WGSL follows the style rules too: no comments. Naga rejects WGSL reserved words as names
  (`target` is one), so a shader that only fails at pipeline creation is caught by the GPU tests.
- GPU tests run headless on the first high-performance adapter (`testing::gpu`, shared through a
  `OnceLock`) and return early when no adapter exists, so GPU-less machines pass. Each pass is
  compared with a CPU reference in `reference.rs` built from the same `TapTable`s and kernels,
  with clamp-to-edge addressing and `f16` rounding of every stored texel, to within a few
  `rgba16float` ulps (`4/1024`). Test images are quantised to `f16` before upload so the
  reference and the GPU see identical inputs. Colour conversion is compared code by code with
  `reference::convert` (f64) after `reference::unpack` reads the frame back through its
  `FrameLayout`, within one code value.
