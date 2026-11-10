---
paths:
  - "crates/rusplay-config/**"
  - "crates/rusplay-core/src/format.rs"
  - "crates/rusplay-va/src/nodes.rs"
  - "crates/rusplay-cuda/**"
  - "crates/rusplay-encode/src/resolve.rs"
  - "crates/rusplay-decode/src/resolve.rs"
  - "crates/rusplay-capture/**"
  - "crates/rusplay-host/**"
  - "crates/rusplay-client/**"
  - "crates/rusplay-app/**"
---

# Automatic settings

A fresh install streams well without touching a setting: every value that depends on the machine,
the display or the source is `auto` by default and is worked out when it is needed. A pinned value
always wins over detection, and a pinned axis always outranks an automatic one.

## Config and migration

- `video.encoder.backend`, `video.decoder.backend`, `video.frame_rate`, `video.codec`,
  `video.pixel_format.chroma` and `video.pixel_format.depth` take `"auto"` (the default), and
  `video.color_mode` is `"source"` (default) or `"configured"`. The types are
  `rusplay-config/src/choice.rs` (`CodecChoice`, `ChromaChoice`, `DepthChoice`,
  `PixelFormatChoice`, `FrameRateChoice` written as `"auto"` or the usual table, `StreamColorMode`)
  plus `VideoEncoderConfig::Auto(AutoEncoderConfig { rate_control, params })` and
  `VideoDecoderConfig::Auto`. Code that needs a concrete value before negotiation reads
  `VideoConfig::stream_codec`/`stream_pixel_format`/`stream_frame_rate`, which give the pinned
  value or the documented placeholder (`UNNEGOTIATED_CODEC`, `UNNEGOTIATED_PIXEL_FORMAT`,
  `UNDETECTED_FRAME_RATE`); the host pins the negotiated values (`VideoConfig::pin_stream`,
  `use_frame_rate`) before it builds the encoder, so encoders never see `auto`.
- Config files carry `version = 3` as their first line (`CONFIG_VERSION`, written by
  `Config::to_toml_string`, removed before deserializing). A file without it is version 1, the
  shape the Settings page used to write in full, so its old shipped defaults cannot be told from a
  choice: `migrate::unpin_legacy_defaults` turns exactly those values into `auto` (a 60 fps rate,
  HEVC, 4:2:2, 8-bit, a VAAPI encoder on `/dev/dri/renderD128` with low power on, a VAAPI decoder on
  that node without CPU copy) and sets `color_mode` to `source` when `video.color` is the default,
  `configured` otherwise. Any changed value stays pinned. From version 2 on those values are read
  as written, so a value the user picks in the new UI is never unpinned. A file older than
  version 3 was written when debanding shipped with thresholds of 3.0 and no bound:
  `migrate::retune_legacy_deband` turns a `scaling.deband.threshold` or `chroma_threshold` of
  exactly 3.0 into the current default (the Settings page writes every field, so the old
  default sits in the file even if it was never touched), and `limit` takes its default as
  any absent key does. A newer version is `VersionUnsupported`.
  The Settings form converts through serde directly and never sees the version key.
- `video.auto_detect_wait_ms` (default 500, 0 to 10000) bounds how long the host waits for the
  first captured frame before it answers a client while the format or colour follows the source.

## Backends and devices

- `rusplay_encode::resolve(&VideoEncoderConfig, preferred_gpu)` and
  `rusplay_decode::resolve(&VideoDecoderConfig, presenting_gpu)` turn `auto` into the first
  candidate whose `capabilities` answer a non-empty format set; a pinned config is returned as is.
  If nothing answers, the config stays `auto` and opening it is `NoEncoderAvailable` /
  `NoDecoderAvailable`. `open` and `capabilities` resolve an `auto` config themselves, so nothing
  panics or guesses.
- Candidates come from `rusplay_va::render_nodes()` (every `/dev/dri/renderD*` with its PCI address
  from `/sys/class/drm/<node>/device`, available without libva) and from CUDA
  (`Driver::device_count`, `Driver::pci_bus_id` through `cuDeviceGetPCIBusId`).
- Encoder order: hardware candidates on the host's wgpu adapter first (`Gpu::pci_bus_id`, matched
  case-insensitively), the rest in enumeration order (VAAPI nodes, then CUDA devices), then
  SVT-AV1, x265, x264; the auto `rate_control` and `params` carry over. An NVIDIA node whose VAAPI
  driver only decodes answers an empty set and is skipped. `HostService::start` resolves once,
  after `Gpu::open`, so every session and the GPU hand-off plan see a concrete backend.
- Decoder order with a window: VAAPI nodes on the presenting GPU (no CPU copy), NVDEC on that GPU,
  NVDEC elsewhere, VAAPI nodes on other or unknown GPUs with `copy_output` (their frames cross
  GPUs through the CPU), then software. Without a window: every VAAPI node in order, then NVDEC,
  then software. `StreamWindow::attach` resolves against the adapter it presents with and exposes
  the result as `StreamWindow::decoder`, which the app writes into the config it connects with, so
  the adapter check, the capability probe and every decoder open agree. `ClientSession` resolves
  once per connect for callers without a window.

## Stream format

- `FormatSet::best_for(FormatPreference)` ranks lexicographically: pinned codec, pinned chroma,
  pinned depth (each as `closest_to` always did), then the automatic depth nearest its target,
  then the widest automatic chroma, then the automatic codec in AV1, HEVC, H.264 order. A fully
  pinned preference ranks exactly like `closest_to` (tested over many sets).
  `FormatPreference::configured(chosen)` fills the automatic axes from the choice, so an automatic
  axis never makes a stream a fallback.
- `rusplay_config::format_preference(codec, pixel_format, source_depth)` is the one mapping from
  config to preference, used by the host's negotiation (`StreamChoice`) and the Settings outlook.
  An automatic depth targets the source: 10-bit when the latest captured frame is a 10-bit fourcc
  or carries PQ/HLG, 8-bit otherwise or before any frame (`SourceTraits` on the `CaptureFeed`,
  updated by the capture stage on every frame, so it survives sessions). With an automatic depth
  the capture offers its 10-bit formats first.
- While `StreamChoice::follows_source` and no frame has been seen, the control stage holds its
  `Hello` answer until the first frame or `video.auto_detect_wait_ms` after the session started.

## Frame rate

- The client announces the refresh of the display its window is on (gpui's
  `Window::refresh_interval`, converted to millihertz; `UNDETECTED_FRAME_RATE` when gpui knows
  none) and announces again when it changes; a pinned `video.frame_rate` is announced as is.
- With an automatic rate the host also waits for the client's `Display` before answering a monitor
  source, and sets `Shared::set_stream_rate` to `virtual_output::stream_rate`: the client's refresh
  capped at `capture.virtual_max_refresh_hz`, lowered to the source's refresh when that is slower.
  The source refresh is the negotiated PipeWire `framerate`, or `maxFramerate` when the stream is
  variable (KWin), published by the capture worker through `SourceRefresh`. Capture cadence,
  send pacing, the quality sampler and the encoder (`PipelineFactory::use_frame_rate`, before
  `build`) all read `Shared::frame_rate`, which is the virtual output's rate, else this stream
  rate, else the configured one. The rate is fixed for the session; a virtual output still follows
  live `Display` changes as before.

## Colour

- `video.color_mode = "source"` makes the stream colour the source's: `GpuPipeline` adopts each
  frame's `ColorSpace` (`follow_source_color`), drops the cached route so it is replanned (usually
  to the encoded path with no mapping) and reopens the encoder when the colour differs from the one
  it was opened with, which starts on a keyframe and announces a new `StreamFormat`. The client maps
  to its display as `color.md` describes. The capture then offers no colour preference, so a
  producer that can describe several encodings sends its own. `"configured"` keeps the previous
  behaviour: every source is converted to `video.color`, which is also the capture's preference.
