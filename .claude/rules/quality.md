---
paths:
  - "crates/rusplay-core/src/quality.rs"
  - "crates/rusplay-core/src/quality/**"
  - "crates/rusplay-host/src/quality.rs"
  - "crates/rusplay-host/src/quality/**"
  - "crates/rusplay-host/src/pipeline/sample.rs"
---

# Quality metrics

- `rusplay_core::measure(source, reconstruction, history, order)` scores one picture: PSNR per
  plane and overall, and XPSNR per plane. `history` is the source luma of the frames before it,
  newest first (`history[0]` is the previous frame, `history[1]` the one before), and
  `TemporalOrder::for_rate` picks first order below 32 fps and second order from 32, as FFmpeg
  does with the integer frame rate. Lossless is `Decibels::IDENTICAL`.
- XPSNR (`quality/xpsnr.rs`) follows FFmpeg's `vf_xpsnr` exactly, quirks included, and
  `xpsnr_matches_ffmpeg_on_the_reference_sequences` (`quality/reference_tests.rs`) holds it within
  0.001 dB of FFmpeg 9.0 on 24 sequences: 8, 10 and 12 bits, 4:2:0, 4:2:2 and 4:4:4, first and
  second order, the min-smoothed small path (352x288 and odd 321x241), HD and odd HD, the
  downsampled path (2560x1440) and its narrow right edge (2530x1460), and pictures too small for
  weighting. The `xpsnr-reference` skill regenerates the scores. What FFmpeg does that is easy to
  get wrong: a block whose activity window is empty (an edge block two samples wide or less)
  keeps weight 1, not the activity floor; in the downsampled path a block whose activity window
  ends 12 samples or less from its left edge gets no spatial activity at all (FFmpeg computes it
  and drops the result), only temporal; the weighted squared error is rounded to an integer
  (`+ 0.5`, truncated) before the decibels, so a weighted error under 0.5 is lossless; and a
  picture whose block size rounds below 4 is plain PSNR while a larger one, however small, is
  weighted. Chroma blocks are the luma blocks scaled by the subsampling, which is what FFmpeg's
  `(b * plane) / luma` sizes come to for every chroma format. FFmpeg's downsampled path reads past
  odd blocks, so odd sizes above 2048x1152 have no exact reference.
- The host samples it (`host.md`, quality sampling): the sampler keeps the luma of the frames
  before the measured one in arrival order and hands them to `measure` newest first
  (`the_history_reaches_the_measurement_newest_first`).
