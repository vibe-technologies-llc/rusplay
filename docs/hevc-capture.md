# Capturing a corrupt HEVC session

Launch the client with `RUSPLAY_HEVC_DUMP_DIR=/tmp/rusplay-hevc rusplay`.
Capture once with `copy_output=false` while the corruption is visible, and once
with `copy_output=true` while audio cuts out. Exit the client normally after each
run. The terminal reports a unique subdirectory for each capture.

Each directory contains `stream.hevc` (the received video content) and
`stream.index` (packet lengths, frame numbers, keyframe flags, capture timestamps,
stream format and client counters). Capture starts at the first keyframe sent to
the decoder and is capped at 64 MiB. A separate writer thread handles disk I/O;
if its bounded queue fills, capture stops instead of blocking decoding or silently
omitting packets. Captures contain the streamed screen content.

To compare a capture through the presentation stage:

```sh
RUSPLAY_HEVC_FIXTURE_DIR=/tmp/rusplay-hevc/REPORTED-SUBDIRECTORY \
RUSPLAY_HEVC_FIXTURE_NAME=stream \
cargo test -p rusplay-client --features software,vaapi --lib \
  vaapi_dmabuf_hevc_motion_presents_like_copied_frames -- --nocapture
```

This compares DMA-BUF and CPU-copy output at native size and scaled to 3072x1728.
It does not exercise the window swapchain or compositor. The replay currently
assumes consecutive, decodable access units; check frame numbers and error
counters in the index if decoding fails. The index does not contain software
reference hashes and cannot be used directly by the decode hash tests.
