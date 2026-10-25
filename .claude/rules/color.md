---
paths:
  - "crates/rusplay-core/src/video.rs"
  - "crates/rusplay-scale/**"
  - "crates/rusplay-client/**"
  - "crates/rusplay-host/**"
  - "crates/rusplay-capture/**"
  - "crates/rusplay-protocol/**"
  - "crates/rusplay-encode/**"
  - "crates/rusplay-decode/**"
  - "crates/rusplay-config/src/video.rs"
  - "crates/rusplay-config/src/scaling.rs"
---

# Colour management

Rusplay is colour managed end to end. The image the player sees should match what the game
produced, as far as the client display can show it. Example target case: a game renders 10-bit
BT.2020 PQ (HDR) on the host, and the client display is Display P3, either SDR or HDR with a
lower peak. The client converts properly. It never just reinterprets the numbers, and it never
clips per channel.

## Ownership

- **The source is described, never assumed.** Capture reports the surface's colour description
  (primaries, transfer, and HDR metadata when present) from PipeWire/portal metadata. sRGB is
  only the fallback for a surface that carries no description.
- **The host preserves the source gamut and transfer.** It does not gamut map or tone map toward
  any particular display, because it doesn't know the display. BT.2020 PQ 10-bit stays BT.2020
  PQ 10-bit on the wire. The one exception is a stream configured to a smaller space than the
  source (for example 8-bit BT.709 for a client that asked for it). The host then maps with the
  same GPU stages the client uses.
- **The stream is self-describing.** The colour description and HDR metadata go in the protocol's
  stream description and in the bitstream (H.264/HEVC VUI `colour_primaries`,
  `transfer_characteristics`, `matrix_coefficients`, `video_full_range_flag`; AV1
  `color_config`; mastering display colour volume and content light level SEI/metadata OBUs).
  The decoder reports what the bitstream says, and that wins over defaults.
- **The client owns the mapping to its display.** The target is the output's image description
  from Wayland `wp_color_management_v1`: primaries, transfer, min/max/reference luminance, and
  the target colour volume. Config can override every field for displays that report nothing or
  report wrongly. With neither, the target is sRGB/BT.709 SDR at 203 cd/m² reference white.

## Model (`rusplay-core`)

- Primaries are a closed enum with CIE xy chromaticities and a white point: BT.709/sRGB,
  Display P3 (D65), DCI-P3 (DCI white), BT.2020/BT.2100, BT.601 525/625, plus `Custom` with
  explicit chromaticities for displays that report their own. RGB↔XYZ matrices are derived from
  the chromaticities in f64. Chromatic adaptation between white points is Bradford. Matrices are
  never hard-coded tables.
- Transfers: sRGB, BT.709/BT.1886, gamma 2.2, PQ (ST 2084), HLG (ARIB STD-B67/BT.2100), linear.
- Y'CbCr matrix and range are separate fields from primaries, following H.273 code points.
- HDR metadata: ST 2086 mastering display (primaries, white point, min/max luminance) and
  CTA-861.3 MaxCLL/MaxFALL, each optional.

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

## Implementation

- Stages are fused into as few dispatches as possible. When the analytic chain gets expensive,
  the colour stages (2 to 4) can be baked into a 3D LUT (`rgba16float`, 65³ by default,
  tetrahedral interpolation), rebuilt only when the source or target description changes. The
  analytic shader stays the reference.
- Math is f32 on the GPU with `rgba16float` intermediates. CPU references in f64 check every
  stage. Tests cover in-gamut colours passing through unchanged, BT.2020 primaries landing on the
  P3 boundary with hue preserved (ΔE_ITP bound), the BT.2390 curve matching the spec's reference
  points, and round trips where the target contains the source.
- Every choice above is a config option with the default named here: target overrides, operator,
  gamut mapping mode, dynamic peak detection, reference white, LUT size and dither.
- Changing the source or target description reconfigures the plan in place, never on the
  per-frame path.
