---
paths:
  - "crates/rusplay-core/src/color.rs"
  - "crates/rusplay-core/src/color/**"
  - "crates/rusplay-scale/**"
  - "crates/rusplay-client/**"
  - "crates/rusplay-host/**"
  - "crates/rusplay-capture/**"
  - "crates/rusplay-protocol/**"
  - "crates/rusplay-encode/**"
  - "crates/rusplay-decode/**"
  - "crates/rusplay-config/src/video.rs"
  - "crates/rusplay-config/src/scaling.rs"
  - "crates/rusplay-config/src/color.rs"
---

# Colour management

Rusplay is colour managed end to end. The image the player sees should match what the game
produced, as far as the client display can show it. Example target case: a game renders 10-bit
BT.2020 PQ (HDR) on the host, and the client display is Display P3, either SDR or HDR with a
lower peak. The client converts properly. It never just reinterprets the numbers, and it never
clips per channel.

## Ownership

- **The source is described, never assumed.** Capture reports the surface's colour description
  (range, matrix, primaries and transfer from the negotiated PipeWire format, with the
  configured `HdrLevels`) on every `Frame::color`. sRGB is only the fallback for a surface that
  carries no description; see `capture.md` for the mapping.
- **The host preserves the source gamut and transfer.** It does not gamut map or tone map toward
  any particular display, because it doesn't know the display. BT.2020 PQ 10-bit stays BT.2020
  PQ 10-bit on the wire. The one exception is a stream configured to a smaller space than the
  source (for example 8-bit BT.709 for a client that asked for it). The host then maps with the
  same GPU stages the client uses.
- **The stream is self-describing.** The colour description and HDR metadata go in the protocol's
  stream description (`ControlMessage::StreamFormat`, `.claude/rules/networking.md`) and in the
  bitstream (H.264/HEVC VUI `colour_primaries`, `transfer_characteristics`,
  `matrix_coefficients`, `video_full_range_flag`; AV1 `color_config`; mastering display colour
  volume and content light level SEI/metadata OBUs, written by every encoder backend,
  `.claude/rules/video-encode.md`). The decoder reports what the bitstream says
  (`DecodedFrame::color`, `.claude/rules/video-decode.md`), and the client lays it over the
  stream description, so the bitstream wins wherever it says something.
- **The client owns the mapping to its display.** The target is the output's image description
  from Wayland `wp_color_management_v1`: primaries, transfer, min/max/reference luminance, and
  the target colour volume. Config can override every field for displays that report nothing or
  report wrongly. With neither, the target is sRGB/BT.709 SDR at 203 cd/m² reference white.
  `rusplay_client::display` does the query and the resolution, below.

## Model (`rusplay-core`)

The model is `rusplay_core::color` (`color.rs` and `color/`), re-exported from the crate root.

- `ColorPrimaries` is a closed enum: `Bt709` (sRGB), `DisplayP3` (D65), `DciP3` (DCI white),
  `Bt2020` (BT.2100), `Bt601_525` (SMPTE 170M), `Bt601_625` (BT.470 B/G), and
  `Custom(Chromaticities)` for displays that report their own and for the sets with no name
  here (`Chromaticities::ADOBE_RGB`, `BT470M`, `GENERIC_FILM`, `EBU3213`). Serde names are kebab-case
  (`display-p3`, `bt601-525`); custom primaries are a table
  `primaries.custom = { red = [x, y], green, blue, white }`.
- A `Chromaticity` stores CIE x and y as `u16` fixed point in the H.273 / ST 2086 step of
  0.00002 (`UNITS_PER_ONE = 50_000`), so `ColorSpace` keeps `Eq` and `Hash` and a value read
  from SEI or a Wayland image description round-trips exactly. `Chromaticities::new` rejects
  collinear primaries and a white point outside the triangle (`ColorError::DegenerateGamut`),
  and serde goes through it, so every value in hand is invertible.
- `Matrix3` (f64) carries the derived matrices: `Chromaticities::rgb_to_xyz` and `xyz_to_rgb`
  come from the chromaticities and the white point, `rgb_to_rgb(target, WhiteAdaptation)`
  composes them with Bradford adaptation (`bradford_adaptation`) or none (absolute
  colorimetric). No matrix is a hard-coded table; tests check the derived ones against the
  published BT.709 and BT.2087 values.
- `TransferFunction`: `Bt709` (BT.709/BT.1886), `Srgb`, `Gamma22`, `Pq` (ST 2084), `Hlg`,
  `Linear`, each with its H.273 code (gamma 2.2 is code 4, BT.470 M). `pq_from_nits` and
  `pq_to_nits` are the f64 PQ pair the CPU side of every stage uses.
- `MatrixCoefficients` (`Bt709`, `Bt601`, `Bt2020Ncl`, H.273 codes 1, 6 and 9, code 5 also
  reading as `Bt601`) is separate from the primaries. `ColorSpace::matrix` is optional and
  `matrix_coefficients()` falls back to the conventional matrix for the primaries, so a config
  that names only `bt2020` still gets BT.2020 NCL. Range stays its own field.
- `HdrLevels` is the one place for luminance levels: `reference_white_nits` (BT.2408 203),
  `peak_nits`, and the optional ST 2086 `mastering: MasteringDisplay` (primaries, `max_nits`,
  `min_nits` as `Luminance` in 0.0001 cd/m² steps, serialised as nits) and CTA-861.3
  `content_light: ContentLightLevel` (`max_cll_nits`, `max_fall_nits`, 0 meaning unknown).
  `content_peak_nits()` is the source peak for tone mapping: MaxCLL, then the mastering
  maximum, then `peak_nits`. The config validates mastering min below max
  (`MasteringLuminanceInverted`).
- `SignalledColor` is what a bitstream said: every field optional (primaries, transfer, matrix,
  range, mastering display, content light). `with_h273_codes` maps codes and leaves unknown or
  unspecified (2) ones unset, `with_full_range` sets the range, and `over(ColorSpace)` replaces
  exactly the fields that were signalled. `ColorPrimaries::from_chromaticities` names a known
  set when the chromaticities match it exactly and is `Custom` otherwise, which is how mastering
  display primaries read from SEI or AV1 metadata come back as `Bt2020` or `DisplayP3`.
- An encoder that cannot signal a description fails with a specific error: custom primaries
  have no H.273 code, so the software encoders return `ColorPrimariesUnsignalable`.

## Client conversion pipeline

It runs on the GPU in wgpu compute, between decode and present, in this order:

1. Y'CbCr → R'G'B': matrix, range and depth from the stream description.
2. EOTF to linear light in absolute cd/m². PQ is absolute. HLG applies the BT.2100 OOTF with the
   system gamma for the target peak. SDR content is scaled so its reference white lands on the
   target's reference white (BT.2408 203 cd/m² by default).
3. **Tone mapping**, when the content peak exceeds the target peak. The content peak comes from
   MaxCLL, then the mastering max, then optional dynamic per-frame peak detection (a compute
   histogram, smoothed across frames, reset on scene cuts). The default operator is the BT.2390
   EETF, applied to the intensity channel in ICtCp/PQ space with chroma scaled to keep saturation.
   Other operators: BT.2446 method A, spline, Reinhard and clip, selected by config.
4. **Gamut mapping** from the source primaries to the target primaries. The default is
   **perceptual**: work in ICtCp (HDR) or Oklab/Oklch (SDR). Hold hue and lightness constant and
   compress chroma toward the target gamut boundary with a soft knee, so colours well inside the
   target gamut are untouched and saturated BT.2020 colours roll off onto the P3 boundary instead
   of clipping. Per-channel clipping shifts hue (saturated greens go yellow), which is why it is
   never the default. Other modes: relative colorimetric (clip in linear target RGB), absolute
   colorimetric, saturation, desaturate, and an optional out-of-gamut highlight for calibration.
   A source gamut that fits inside the target is a plain matrix, with no mapping step.
5. Scaling (`rusplay-scale`) runs on the converted, display-referred linear signal, so filters
   and anti-ringing see the values that will be shown and HDR highlights don't ring.
6. Inverse EOTF of the target and quantisation to the swapchain depth with dithering (blue noise
   or ordered, configurable). A 10-bit or float swapchain is used whenever the compositor offers
   one.

## Mapping stages (`rusplay-scale::color`)

- `ColorMapper` is one compute pass (`color_map.wgsl` with the `color_common.wgsl` and
  `color_gamut.wgsl` preludes) from an input texture to an `rgba16float` storage texture of the
  same size. Its contract: input is linear light in the source primaries with 1.0 at the
  source's reference white; output is linear light in the target primaries with 1.0 at the
  target's reference white, inside `[0, ColorPlan::target_peak]` (`peak / white` for an HDR
  target, 1 for SDR) except on the matrix path. Alpha passes through. It is the stage between
  linearisation and scaling, so the scaler sees display-referred values.
- `ColorMapSettings { source, target, gamut }` describes both ends as `ColorVolume`s
  (primaries, transfer, reference white, peak and black in cd/m²).
  `ColorVolume::of_content(&ColorSpace)` takes the peak from `HdrLevels::content_peak_nits` and
  the black from the mastering minimum; `srgb_display()` is the fallback target. The transfer
  only decides HDR versus SDR: an SDR source's reference white is taken to be the target's.
- `ColorPlan::new` does all the f64 work once per settings value: Bradford-adapted matrices
  through D65 XYZ (the working white), the path, the working space and the inverses, written
  into one params uniform. `ColorMapper::configure` builds a new plan and uniform off the frame
  path and reports `Reconfigured::Parameters`; nothing is allocated per frame except bind
  groups for frame views not seen recently.
- The path (`MappingPath`) is chosen on the CPU. A source whose relative matrix into the target
  has no negative coefficient fits inside it, and `perceptual`, `relative`, `desaturate` and
  `highlight` all become `Matrix` (no clamp, no mapping). Otherwise: `perceptual` is
  `Perceptual`; `relative` is `Clip` (Bradford matrix, clamp per channel in target RGB);
  `absolute` is `Clip` with the unadapted matrix, so the source white keeps its chromaticity;
  `saturation` is `Clip` with the identity, reinterpreting source RGB as target RGB;
  `desaturate` mixes each colour toward its target luminance (clamped to the peak) just enough to
  fit, keeping luminance where it can; `highlight` shows in-gamut colours relative colorimetric
  and replaces out-of-gamut ones with `peak - clipped`.
- Perceptual mapping runs in `WorkingSpace::Ictcp` when either end is HDR and
  `WorkingSpace::Oklab` otherwise. ICtCp uses the BT.2100 LMS matrix composed with the derived
  XYZ to BT.2020 matrix, PQ on absolute luminance (the target white times the relative value);
  Oklab's XYZ to LMS rows are normalised so D65 lands exactly on the neutral axis. Lightness and
  hue are held; for the colour's (lightness, hue) `chroma_boundary` finds the target gamut's
  boundary chroma `T` along the constant-lightness, constant-hue ray, returns the colour
  unchanged if `C <= knee * T`, and otherwise finds the source gamut's boundary `S` the same way
  and compresses with the ACES reference gamut compression curve (power `COMPRESSION_POWER` =
  1.2) so `knee * T` keeps slope 1 and `S` lands exactly on `T`. A colour beyond `S` is clipped
  to `T`, and the result is clamped to the target cube, which also absorbs the boundary's soft
  margin. `knee` is `GamutMapping::knee`, default 0.8, validated into `0..1`.
- The boundary is a soft first exit, continuous in lightness and hue. A ray can graze a face of
  the cube: in the blue-violet of both spaces (around 329° in ICtCp, 264° in Oklab) it runs
  almost along the red = 0 face, leaves it by up to about 2% of the grey's distance to the
  face, re-enters and leaves again, and there the first exit, the last exit and a bisection
  (the previous design) all jump by up to 15% of `T` between neighbouring colours, an edge in
  any saturated gradient. A margin only moves that jump to wherever the excursion reaches it.
  `chroma_boundary` instead integrates over chroma a weight that falls linearly from 1 to 0 as
  the running maximum of the excess goes from half a tolerance inside the face to half a
  tolerance beyond it (`BOUNDARY_RAMP_CENTRE` = 0.5 of a tolerance). The excess is per bound
  and relative to the grey's own distance to it: `BOUNDARY_LOWER_TOLERANCE` = 2% at 0,
  `BOUNDARY_UPPER_TOLERANCE` = 0.2% at the peak (no ray found grazes an upper face, and an
  overshoot there clamps a bright channel). A sharp exit integrates to the exact face; a
  grazing one ends at most half a tolerance out, which the clamp absorbs (clamp distortion p99
  under 0.1 ΔE_ITP, at most 0.3); an excursion counts in proportion to its depth, so a
  shallow one moves `T` halfway between the inner and the outer exit and a deep one gives the
  inner exit, every transition continuous. The boundary stays steep where the fold is (the
  inner exit itself races toward the outer one near the tangency), up to about 9% of the peak
  per 10-bit code in blue to magenta gradients (`color-perceptual-fold-steepness` in
  `docs/BUGS.md`). Each channel along the ray is a cubic Hermite
  segment between exact samples with analytic derivatives (the PQ or cube derivative through
  `lab_to_lms`, composed with the target or source matrix), integrated at
  `BOUNDARY_SUBSTEPS` = 4 points per segment with the crossing found by linear interpolation; a
  segment whose Bernstein hull is inside is taken whole, and the integral stops once the weight
  is 0. A coarse pass of `BOUNDARY_COARSE_STEPS` = 4 segments over `0..min(BOUNDARY_REACH (3)
  × the tangent line's exit, the chroma limit)` places a fine pass of `BOUNDARY_APPROACH_STEPS`
  = 2 segments to `BOUNDARY_WINDOW_START` (0.75) × the coarse result and
  `BOUNDARY_WINDOW_STEPS` = 4 to `BOUNDARY_HEADROOM` (1.25) × it. The chroma limits
  (`ColorPlan::target_chroma_limit`, `source_chroma_limit`, the params uniform's
  `chroma_limits`) are 1.2 × the largest chroma on a 33² grid of each cube face. Where the
  boundary is steep, f32 noise moves the GPU result by up to three `rgba16float` steps from the
  f64 reference.
- Tone mapping runs in the same dispatch, before gamut mapping (`color_tone.wgsl`), when the
  source is HDR and either its peak exceeds the target's display peak (`peak_nits` for an HDR
  target, the reference white for SDR) or dynamic peak detection is on; `ColorPlan::tone`
  then holds a `ToneStage` (`ToneCurve` plus the optional `DynamicPeak`). The source peak is
  `ColorVolume::peak_nits`, which `of_content` fills from MaxCLL, then the mastering maximum,
  then `video.color.hdr.peak_nits`; the source black is the mastering minimum. The pixel goes
  to D65 XYZ in absolute cd/m², to ICtCp (the BT.2100 LMS matrix over the derived XYZ to
  BT.2020 matrix, PQ), the operator maps the intensity `I` as a PQ code, `Ct` and `Cp` are
  scaled by `min(I'/I, I/I')` to keep saturation, and the result returns to source RGB relative
  to the target white for the gamut stage. Hue in ICtCp is untouched by construction.
- Operators (`ToneMapOperator`, all curves cd/m² to cd/m² that clip at the target peak, f64 in
  `ToneCurve::map_pq`/`map_nits` and f32 in the shader): `bt2390` (default) is the BT.2390
  EETF in normalised PQ with `KS = 1.5 maxLum - 0.5`, the Hermite roll-off and the
  `minLum (1 - E2)^4` black lift (the lift raises the peak by under 0.1%); `bt2446a` is
  BT.2446 method A's luminance curve (ρ from both peaks, the three-piece tone curve, gamma
  2.4 out); `spline` is identity in normalised PQ below `0.7 maxLum` and a cubic Hermite with
  zero end slope onto `maxLum` above, its start slope capped at three times the secant so it
  stays monotonic; `reinhard` is extended Reinhard with the source peak as white; `clip` clips.
- Dynamic peak detection (`DynamicPeak`, off by default) adds two dispatches before the map:
  `color_histogram.wgsl` (16x16 workgroups, one workgroup-local atomic bin per invocation)
  counts `PQ(max(R, G, B))` into `HISTOGRAM_BINS` = 256 bins, and `color_peak.wgsl` (one
  invocation) takes the bin edge at `percentile` (99.995), smooths it in PQ as
  `prev + (frame - prev) / smoothing_frames` (20), resets to the frame value when the mean
  luminance moved by more than `scene_threshold_db` (3 dB, `10 log10` of the mean's nits)
  since the previous frame, clamps to `[target peak, static source peak]`, rebuilds the curve
  and clears the bins. The curve and state live in a GPU storage buffer
  (`ColorMapper::tone_state`), written once from the CPU for a static curve and by the peak
  pass for a dynamic one, so nothing is read back and the frame path never waits.
  Reconfiguring resets the state.
- `color/reference.rs` is the f64 mirror of the shader. Tests check that greys and colours
  below the knee pass through, that BT.2020 primaries and secondaries map onto the P3 boundary
  with lightness held, hue within 1° and a hue-only ΔE_ITP below 1 (ICtCp), that chroma
  compression is monotonic and ends on the boundary, the desaturate and highlight semantics,
  and that the GPU matches the reference for every mode and both spaces within the
  `rgba16float` tolerance. `color/boundary_tests.rs` checks continuity: the worst step of a
  2048-step sweep along saturated BT.2020 gradients (blue to magenta and cyan, four targets)
  must shrink 8x when refined 256x, which the old bisection failed in 39 of 84 sweeps; the
  grazing codes (0.28125, 0.23338, 0.546875) and green + 0.0005 map within 0.02 (0.722 and
  0.712 blue, against 0.777 and 0.683 before) on CPU and GPU; mapped chroma grows
  monotonically through the fold; and the GPU follows the reference along the gradients. Tone tests check BT.2390 against its reference points (the
  4000 to 1000 cd/m² maxLum 0.83298 and KS 0.74947, identity below KS, slope 1 at the knee,
  source peak onto target peak, the black lift), that every operator is monotonic and lands on
  the target peak, that hue is held and chroma scales with the intensity ratio, that the
  motivating BT.2020 PQ to SDR Display P3 case stays in range with hue within 2°, GPU parity
  for every operator, and the dynamic state machine (first frame, smoothing, scene cut)
  against `reference::detect_peak`.
- Cost at 3840x2160 on an RX 9070 (`bench.rs`, a per-pixel noise pattern that keeps every wave
  on the slow path): perceptual 1.8 ms SDR and 2.9 ms HDR (PQ with its derivative at
  every ray sample), relative 0.23 ms; the soft boundary costs about 1.4x (HDR) to 2x (SDR)
  the bisection it replaced. BT.2390 tone mapping adds about 0.1 ms and dynamic peak detection
  about 0.18 ms. A whole 10-bit 4:2:0 BT.2020 PQ frame (unpack, `Lineariser`, perceptual BT.2390
  `ColorMapper`) is 3.8 ms; through the LUT below it is 0.58 ms at 65³ (0.61 ms at 97³,
  0.63 ms at 129³), against 0.27 ms for the unpack alone, plus a one-off bake of about 0.22
  ms (0.45, 0.85).
- `ColorLut` (`color/lut.rs`, `color_lut_bake.wgsl`, `color_lut_storage.wgsl`,
  `color_lut_sample.wgsl`) bakes linearisation, tone mapping and gamut mapping into one
  `size`³ `rgba16float` 3D texture indexed by the frame's encoded R'G'B' (the PQ, HLG or SDR
  code itself, so the shaping is the source's own). `ColorLutSettings` names the source
  transfer, `HdrLevels` and matrix (the `Lineariser` inputs) and an optional
  `ColorMapSettings`; the bake is one dispatch per lattice point running `decode_signal`
  (`transfer_decode.wgsl`, shared with the lineariser) and `map_light` (`color_light.wgsl`,
  shared with `color_map.wgsl`) against the very params uniform and tone state `ColorMapper`
  builds, so the LUT is the analytic shader sampled. It runs from `encode_bake` in the frame's
  own command buffer, once after `new` or a `configure` that changed the settings
  (`Reconfigured::Tables`, a new texture). Values are stored through a sign-symmetric 1/2.4
  power (with the sRGB linear toe for SDR sources and for signed matrix output), which keeps
  greys and dark colours close to linear in the lattice. Final clamps stay analytic: the
  `Clip` path is baked unclamped as `Matrix` and every mapped path clamps to
  `[0, target_peak]` after the lookup, so the clamp's kink is not interpolated. Dynamic peak
  detection (`LutDynamicPeak`) and `highlight` (`LutHighlightMapping`, discontinuous by design)
  cannot be baked; sizes are `LUT_SIZES` (2 to 256, and the device's 3D limit).
- The lookup is fused into `RgbConverter` (`encode_with_lut`, `encode_planes_with_lut`):
  `unpack_rgb.wgsl` with `unpack_output_lut.wgsl` replaces the inverse OETF by a tetrahedral
  interpolation of the eight surrounding lattice texels (`textureLoad`, four per sample), so a
  frame takes one source-resolution pass instead of three. The converter must output
  `Encoded` (`LutInputNotEncoded`) for the LUT's transfer (`LutTransferMismatch`).
- LUT accuracy against the analytic passes (`color/lut/tests.rs`: 2^17 random R'G'B' codes plus
  saturated BT.2020 primaries and secondaries from 1 to 4000 cd/m², 4:4:4 12-bit, ΔE_ITP per
  BT.2124, codes with every channel below 0.03 left out as sub-display black): at 65³ greys and
  smooth mappings (linearise only, SDR relative) are within 0.8; gamut-mapped HDR is mean
  0.11 to 0.23, p99 1.0 to 2.6, with isolated maxima of 15 to 37 at the gamut cusps (the
  continuous boundary barely moved these: the error is the lattice interpolating across the
  boundary's cusps and folds, not a jump). 97³ halves p99 (at most 1.5). About 0.1 to 0.2 of the mean is the analytic
  path's own `rgba16float` intermediate, which the fused pass does not have. Because p99 exceeds
  one JND near the gamut boundary, the analytic passes stay the default.
- Config: `color.tone_mapping.operator` (`bt2390`), `dynamic_peak` (false), `peak_percentile`
  (99.995), `smoothing_frames` (20) and `scene_threshold_db` (3.0);
  `color.gamut_mapping.mode` (default `perceptual`) and `knee` (0.8);
  `color.processing.method` (`analytic` default, `lut`) and `lut_size` (65, 17 to 129); and the display
  overrides `color.target.primaries`, `transfer`, `reference_white_nits`, `peak_nits` and
  `black_nits`, all unset by default. Resolving them is `rusplay_client::display` (next
  section); the client presentation below turns the result into `ColorMapSettings`.

## Display target (`rusplay-client::display`)

- `query_outputs` opens its own pure-Rust `wayland-client` connection (`wayland-protocols` with
  `client` and `staging`), binds `wp_color_manager_v1` (versions 1 to 3) and every `wl_output`
  (name from version 4), asks each output for its image description and its information, and
  returns one `OutputDisplay { name, image }` per output, where `image` is a per-output
  `Result<DisplayImage, ClientError>` (`ImageDescriptionFailed`, `ImageInformationIncomplete`).
  Two roundtrips are enough because the compositor answers `get_image_description` and
  `get_information` immediately. A compositor without the protocol gives
  `ColorManagementUnsupported`; the caller then resolves `DisplayImage::default()` against the
  overrides to get the sRGB fallback. `query_display(name)` picks an output by `wl_output` name
  (the first when `None`) and `query_target(name, overrides)` also resolves. Which output the
  window is on is the caller's to say; the module has no window.
- `DisplayImage` is the pure, protocol-free answer: `primaries` (primary colour volume),
  `target_primaries` (the displayable gamut), `transfer`, `luminances` (min, max, reference
  white) and `target_luminance` (min, max). Wire values are converted by pure functions
  (`primaries_from_chromaticity`, 1e-6 units, matching a named set exactly or becoming
  `Custom`; `primaries_from_named`; `transfer_from_named`, with `compound_power_2_4`, `srgb` and
  `ext_srgb` all `Srgb`; `transfer_from_power`, only exponents 2.1 to 2.3 as gamma 2.2;
  luminance conversions). Anything the model cannot hold (gamma 2.8, log, ST 240/428, XYZ
  primaries, luminances outside 1 to 10000 cd/m², degenerate chromaticities) is left unreported
  rather than approximated.
- `resolve_target(&DisplayImage, &DisplayTargetConfig) -> Result<ColorVolume, ClientError>`
  is pure. Per field the override wins, then the report, then the default. Gamut: override,
  `target_primaries`, `primaries`, BT.709. Transfer: override, reported, sRGB. Reference white:
  override, reported, 203. Peak: override, `target_luminance` max, `luminances` max, then 1000
  for an HDR transfer or the reference white for an SDR one. Black: override, `target_luminance`
  min, `luminances` min, 0. A black level at or above the peak is
  `TargetBlackNotBelowPeak`, since the overrides are validated only against each other.

## Client presentation (`rusplay-client`)

- Per frame the present stage runs unpack, then `Lineariser` for PQ/HLG frames (`OutputLight::Linear`;
  for HLG on an HDR target the OOTF uses the target's peak), then `ColorMapper` unless the plan
  is identity, then `Scaler`, all at the source resolution except the scaler's output. With
  `color.processing.method = "lut"` the route instead carries `ColorLutSettings` whenever it
  would run a lineariser or a mapper and the plan is bakeable (no dynamic peak, not
  `highlight`): the frame is unpacked `Encoded` straight through the `ColorLut` and takes no
  colour pass (`ColorRoute::passes` is 0, `ColorRoute::unpack` says how to unpack).
  `ColorRoute::plan` (`present/mapping.rs`) builds the `ColorMapSettings` from
  `ColorVolume::of_content` of the frame's colour, the colour target and `MappingOptions` (the
  `color.tone_mapping` and `color.gamut_mapping` sections, the same conversion the host makes),
  and `ColorStage` caches it until the frame colour or the target changes. The texture handed to
  the sink is linear light in the target primaries with 1.0 at the target's reference white, and
  its `FrameColor` says so (`client.md`).
- The window picks its surface at attach and again whenever the output it is on or that output's
  description changes (`window/output.rs`, `SurfaceOutput::choose`, pure and unit tested).
  `color.output.query_display` (on by default) asks the compositor for the first output's image
  description (`query_display(None)`) at attach and starts the output follower (below); an answer
  makes the display `Managed` (the description with the overrides laid over it), no answer
  `Unmanaged` (the overrides over the sRGB fallback). The surface colour spaces, in `auto` order:
  - managed HDR display: `Bt2100Pq` (BT.2020 primaries, PQ with the target's reference white
    written as 203 cd/m², the protocol's default PQ reference, which the compositor maps onto the
    output's own reference white), then `ExtendedSrgbLinear`, then sRGB;
  - managed Display P3 SDR display: `DisplayP3` (sRGB transfer), then scRGB, then sRGB; any other
    gamut wider than BT.709: scRGB, then sRGB;
  - otherwise sRGB. On a managed compositor an sRGB surface's target is sRGB/BT.709 SDR at the
    display's reference white whatever the display is, since the compositor converts from sRGB;
    on an unmanaged one the target is the overrides, encoded verbatim (primaries, transfer, PQ
    in absolute cd/m²) into the untagged buffer, which is how a user describes a panel that
    receives pixels unconverted.
  `ExtendedSrgbLinear` (scRGB) is the target's linear light converted to BT.709 primaries with
  1.0 at reference white and nothing clipped, negative values carrying the wider gamut. Formats
  per space: `Rgb10a2Unorm`, `Bgra8Unorm`, `Rgba8Unorm` for sRGB and Display P3,
  `Rgba16Float` for scRGB, `Rgb10a2Unorm` then `Rgba16Float` for PQ, each only where the surface
  capabilities list that colour space for that format (Mesa's Wayland WSI should offer the wide
  ones only on a colour-managed compositor and tag the surface itself; not yet checked against a
  running HDR compositor). `color.output.color_space` (`auto`,
  `srgb`, `display-p3`, `extended-linear`, `pq`) and `color.output.format` (`auto`, `rgba8`,
  `rgb10a2`, `rgba16-float`) force a choice.
- Encoding is the last step of `compose.wgsl`, after the cursor blend: the target-to-signal
  matrix (Bradford), then the signal's transfer: sRGB, BT.709 OETF, gamma 2.2, PQ of
  `light * white / 10000`, HLG (BT.2100 inverse OOTF at the signal's peak with the signal
  primaries' luma row, then the OETF), linear clamped to `0..1`, or scRGB unclamped.
- Dithering (`color.output.dither`: `blue-noise` default, `ordered`, `off`) quantises in the
  shader as `floor(encoded * (2^bits - 1) + t) / (2^bits - 1)` with the threshold `t` from the
  pattern, 8 bits for the 8-bit formats, 10 for `Rgb10a2Unorm`, none for float. Doing the floor
  itself keeps the result independent of the hardware's float-to-unorm rounding. `ordered` is a
  16x16 Bayer matrix computed from the pixel's bits; `blue-noise` is a 64x64 void-and-cluster
  ranking (Gaussian σ 1.5 on the torus, 10% initial fill from a fixed xorshift seed, tightest
  cluster and largest void found from incrementally updated energies), generated once per
  process (`window/dither.rs`) and uploaded as `r32float`. The pattern is static across frames.
- Tests: GPU against f64 CPU references, in `present/mapping/tests.rs` (BT.2020 PQ greys onto
  an sRGB display follow `ToneCurve::map_nits`; saturated PQ and SDR BT.2020 colours land in
  the sRGB gamut with ICtCp or Oklab hue within 3° or 2° and never further than per-channel
  clipping moves it, lightness held, an in-gamut colour untouched; a source that matches the
  display takes no mapping pass) and `window/compose/tests.rs` (sRGB and PQ codes against the
  CPU transfer functions, scRGB keeping negative and above-1 values, both dither patterns
  matching `floor(v + t)` per pixel with the mean on the level, and an identity chain, PQ
  through the lineariser and sRGB, reproducing its source codes).
- Following the output (`window/follow.rs`, `window/choice.rs`): `ChildSurface` records the
  `wl_output`s from `wl_surface.enter`/`leave` on the child surface (the proxies are the
  toolkit's own bindings, so their names are not readable and no `query_outputs` match is
  needed). The `rusplay-client-output` thread, which exists only when `query_display` is on and
  the compositor has `wp_color_manager_v1`, dispatches that queue and its own, and polls the
  socket every 50 ms the way the input capture thread does. It follows one entered output
  (`follow_choice`): the current one while the surface is still on it, otherwise the most
  recently entered, and the last one when the surface is on none. For the followed output it
  holds a `wp_color_management_output_v1` and requests the image description again on
  `image_description_changed` and whenever the followed output changes; stale answers are
  dropped by a generation counter. A finished `DisplayImage` goes to the presenter through
  `Pending::display`, so the presenter thread is the only one that touches the swapchain.
- `OutputChoice::observe` resolves the new image against the `color.target` overrides and, only if
  the resulting `Display` differs, runs `SurfaceOutput::choose` again. An image that does not
  resolve (`TargetBlackNotBelowPeak`) or fits no swapchain keeps the last good choice. A changed
  `SurfaceOutput` is staged and its target is published at once to `WindowSink::color_target`,
  so the present stage maps new frames to it (`ColorStage` replans on the changed target). The
  presenter keeps the old swapchain and `Composer` encoding until the first frame whose
  `FrameColor` equals `mapped_to` the staged target arrives (`OutputChoice::admit`), then
  reconfigures the surface and calls `Composer::set_encoding` in the same draw. Frames still in
  flight for the old target keep drawing with the old encoding; a frame matching neither is
  dropped and the previous one stays. Nothing waits on the compositor, and a static screen
  keeps its picture until the next frame.
- HDR metadata (ST 2086, MaxCLL) is not passed to the compositor: wgpu has no
  `vkSetHdrMetadataEXT`.

## Implementation

- Stages are fused into as few dispatches as possible. The colour stages (2 to 4) can be baked
  into a 3D LUT (`rgba16float`, 65³ by default, tetrahedral interpolation) fused into the
  unpack, rebuilt only when the source or target description changes (`ColorLut` above,
  opt-in through `color.processing.method`). The analytic shader stays the reference and the
  default; `client-color-lut-boundary` in `docs/PERFORMANCE.md` is what it takes to flip it.
- Math is f32 on the GPU with `rgba16float` intermediates. CPU references in f64 check every
  stage. Tests cover in-gamut colours passing through unchanged, BT.2020 primaries landing on the
  P3 boundary with hue preserved (ΔE_ITP bound), the BT.2390 curve matching the spec's reference
  points, and round trips where the target contains the source.
- Every choice above is a config option with the default named here: target overrides, operator,
  gamut mapping mode, dynamic peak detection, reference white, surface colour space and format,
  dither, and the processing method and LUT size.
- Changing the source or target description reconfigures the plan in place, never on the
  per-frame path.
