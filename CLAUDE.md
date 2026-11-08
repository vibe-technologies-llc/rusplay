# Rusplay

Rusplay is a LAN remote play app for Linux: one machine hosts (captures, encodes, streams) and
another connects (receives, decodes, presents, forwards input). It is Wayland only, aims for latency
and image quality well beyond the usual streaming look (4:2:2/4:4:4 chroma, 10-bit, proper GPU
scaling filters), and treats every tunable as a configuration option.

## Architecture

A Cargo workspace of `crates/rusplay-*`, Rust edition 2024 on the current stable toolchain.

| Crate | Role |
|---|---|
| `rusplay-core` | Shared value types: resolution, frame rate, codecs, chroma, colour, scale filters, timestamps, the `Frame` buffer types (`rusplay-capture` re-exports them) |
| `rusplay-config` | The serde/TOML configuration tree, its defaults and validation |
| `rusplay-protocol` | Versioned wire format, no I/O |
| `rusplay-net` | Raw UDP transport, pacing, FEC, reliable control channel |
| `rusplay-priority` | Frame-thread scheduling: nice value, timer slack, rtkit realtime fallback |
| `rusplay-va` | libva wrappers shared by VAAPI encode and decode: bindgen bindings, display, config, context, buffers, surfaces, images (feature `libva`) |
| `rusplay-cuda` | `libcuda.so.1` loading and the driver symbols shared by NVENC and NVDEC, loaded at run time |
| `rusplay-capture` | xdg ScreenCast portal + PipeWire capture into DMA-BUF or shared-memory frames |
| `rusplay-scale` | GPU (wgpu compute) resampling with Hermite, Lanczos, EWA and other kernels |
| `rusplay-encode` | Video encoder trait and backends: VAAPI, NVENC, x264, x265, SVT-AV1 |
| `rusplay-decode` | Video decoder trait and backends: VAAPI, NVDEC, software (rav1d AV1, libavcodec H.264/HEVC) |
| `rusplay-audio` | Opus (opus-rs) and optional FLAC audio codecs |
| `rusplay-clipboard` | Text clipboard sync: the pure engine, `ClipboardLink`, data-control (ext and wlr) and portal backends (features `wayland`, `portal`) |
| `rusplay-input` | Input event model, host injection, client capture |
| `rusplay-host` | Host session: capture → scale → encode → net, plus input injection |
| `rusplay-client` | Client session: net → decode → scale → present, plus input capture |
| `rusplay-hostd` | Headless host binary |
| `rusplay-app` | The gpui desktop app, binary `rusplay`: connects to hosts and hosts this machine through `rusplay-host`'s `HostService` |

Dependency direction: `core` sits at the bottom. `config`, `protocol` and `scale` build on it,
`net` builds on `protocol` and `config`, `priority` builds on `config`, `va` and `cuda` build on `core` (`cuda` on nothing else),
and the media crates (`capture`, `encode`, `decode`, `audio`, `input`) build on `core`/`config`; `encode` and `decode` also
use `va` and `cuda`, which is how the two share driver code without depending on each other. `clipboard` builds on `config` and `protocol` (its portal backend also on `input`); `host` and `client` compose them and `priority`, and the binaries
depend only on `host`/`client`/`config` (`hostd` on `host`, `app` on both). No crate depends upward, and no media crate depends on
`net`. `vendor/rav1d` is a patched copy of rav1d, outside the workspace and outside the style checks (`.claude/rules/dependencies.md`). `fuzz/` is a cargo-fuzz crate outside the workspace (its own `[workspace]`, so `--workspace`
commands skip it) that feeds arbitrary bytes to the `protocol`, `net` and `input` decoders and to
the `decode` bitstream parsers (through its `fuzzing` feature).

## Invariants

- Wayland only. No X11 code paths, no XWayland assumptions. Games under Wine/Proton are captured
  through the compositor like any other surface.
- Latency first. Nothing on the frame path blocks on a reliable channel, buffers a frame it could
  send, or copies a frame it could hand over as a DMA-BUF.
- Every tunable lives in `rusplay-config` with a default; nothing is hard-coded that a user could
  reasonably want to change.
- Chroma above 4:2:0 and bit depth above 8 are first-class paths, not fallbacks.
- Scaling is always done with Rusplay's own GPU kernels, never by a driver's built-in scaler.
- Colour is managed end to end: the source's colour description travels with the stream, and the
  client tone maps and gamut maps (perceptually by default) to its display, never clipping per
  channel.
- Stub code never panics: unimplemented paths return a specific error variant.

## Commands

| Task | Command |
|---|---|
| Build | `cargo build --workspace --all-features` |
| Lint | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| Test | `cargo test --workspace --all-features` |
| Format | `rust-formatter` (never `cargo fmt`/`rustfmt`) |
| Format check | `rust-formatter --check` |
| Comment scan | `scripts/check-comments.sh` |
| Nested KWin run | `cargo test -p rusplay-hostd --features pipewire,svt-av1 --test nested -- --ignored --test-threads=1` |
| Hardware test run | `RUSPLAY_REQUIRE_HW=1 cargo test --workspace --all-features` |
| Fuzz a decoder | `cargo +nightly fuzz run <target>` in `fuzz/` (`discovery`, `pairing`, `reconnect`, `control`, `clipboard`, `cursor`, `input`, `headers`, `reassembly`, `decode_h264`, `decode_hevc`, `decode_av1`, `decode_software`, `decode_color`); the `decode_*` targets run with `-O` (release arithmetic) |
| Release / profiling build | `cargo build --release` / `cargo build --profile profiling` |
| Daemon with encoders and PipeWire | `cargo build --release -p rusplay-hostd --features vaapi,nvenc,software-encoders,pipewire,input,flac` |
| App with decoders and host encoders | `cargo build --release -p rusplay-app --features vaapi,nvdec,software,ffmpeg,input` |

Tests that need a GPU, a VAAPI render node or an NVIDIA driver skip when it is missing, writing
`skipped <test>: <reason>` straight to stderr so `cargo test` shows it despite output capture. With
`RUSPLAY_REQUIRE_HW=1` a missing device fails the test instead; a device that exists but lacks a
codec, chroma or bit depth still skips visibly. The skip logic is `src/hardware.rs` in each crate
with such tests, included by path from that crate's integration tests.

Profiles: dev builds at `opt-level = 1`. Release uses thin LTO, one codegen unit, stripped
symbols and `panic = "abort"`. `profiling` is release with debug info, no stripping and unwinding
panics. `.cargo/config.toml` builds x86_64 with `target-cpu=native`.

Commits are `type: summary` with `feat`, `bugfix`, `chore`, `refactor`, `perf`, `docs` or `test`,
with no trailers or attribution lines. The full pre-commit procedure is the `check` skill.

## Roadmap

`docs/ROADMAP.md` indexes the work lists (`docs/FEATURES.md`, `docs/BUGS.md`,
`docs/PERFORMANCE.md`, `docs/QUALITY.md`). An item is deleted from its list in the same commit
that implements it. The format is in `.claude/rules/roadmap.md`.

## Rules

| File | Covers |
|---|---|
| `.claude/rules/style.md` | Code style: no comments, layout, imports, sync primitives, hashing |
| `.claude/rules/errors.md` | Error enums |
| `.claude/rules/dependencies.md` | Adding and versioning dependencies, pure-Rust policy, C-backed allowlist |
| `.claude/rules/roadmap.md` | Roadmap file format and upkeep |
| `.claude/rules/config.md` | Configuration tree, defaults, validation |
| `.claude/rules/capture.md` | Portal and PipeWire capture |
| `.claude/rules/scaling.md` | GPU scaling kernels and pipeline |
| `.claude/rules/color.md` | End-to-end colour management: description, signalling, tone and gamut mapping |
| `.claude/rules/video-encode.md` | Encoder backends, chroma, latency settings, parameter passthrough |
| `.claude/rules/video-decode.md` | Decoder backends and presentation hand-off |
| `.claude/rules/audio.md` | Audio codecs, threads, resampling |
| `.claude/rules/networking.md` | Wire protocol and UDP transport |
| `.claude/rules/priority.md` | Thread scheduling for the host and client frame threads |
| `.claude/rules/driver-wrappers.md` | `rusplay-va` and `rusplay-cuda`: libva and libcuda wrappers shared by encode and decode |
| `.claude/rules/input.md` | Input forwarding and injection |
| `.claude/rules/clipboard.md` | Text clipboard sync: direction, limits, echo suppression, backends |
| `.claude/rules/ui.md` | The gpui desktop app |
| `.claude/rules/ui-design.md` | The app's visual system: palette roles, one accent, no shadows or pills, tabular figures, stable layouts, the contrast test |
| `.claude/rules/hostd.md` | Headless host binary: arguments and config discovery |
| `.claude/rules/client.md` | Client session: stages, feedback, frame hand-off and colour |
| `.claude/rules/host.md` | Host session: accept, stages, keyframes, GPU capture → encode pipeline |
| `.claude/rules/quality.md` | PSNR and XPSNR: the FFmpeg-exact XPSNR, its reference sequences, host sampling |

## Skills

`check` (full pre-commit run), `commit`, `add-dependency`, `add-crate`, `add-config-option`,
`xpsnr-reference`.
