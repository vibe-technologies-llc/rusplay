---
paths:
  - "**/Cargo.toml"
---

# Dependencies

- Every third-party dependency is declared once in the root `[workspace.dependencies]` with a full
  three-part version (`serde = "1.0.229"`) and `default-features = false` when its defaults
  carry more than we use. Member crates use `name.workspace = true` and add only the features
  they need.
- A newly added dependency gets the latest published version, looked up with `cargo search` or
  `cargo info` at that moment, never recalled from memory. The `add-dependency` skill is the
  procedure.
- No `repository`, `authors` or `homepage` fields, and no personal details anywhere in
  manifests.
- **Pure Rust first.** A C-backed crate is used only where no Rust implementation covers the
  need, and always behind a cargo feature of the crate that uses it, so the default build stays
  pure Rust. The allowed C-backed set, added as each backend is implemented:

  | Need | Crate | Feature |
  |---|---|---|
  | PipeWire stream consumption | `pipewire` | `rusplay-capture/pipewire` |
  | VAAPI encode/decode | `cros-libva` | `rusplay-encode/vaapi`, `rusplay-decode/vaapi` |
  | NVENC / NVDEC | `nvidia-video-codec-sdk` | `rusplay-encode/nvenc`, `rusplay-decode/nvdec` |
  | x264 | `x264` bindings | `rusplay-encode/x264` |
  | x265 | `x265-sys` bindings | `rusplay-encode/x265` |
  | SVT-AV1 | `svt-av1-enc` or own bindings | `rusplay-encode/svt-av1` |

  Pure-Rust choices already made: `opus-rs` (Opus), `flacenc` (FLAC, `rusplay-audio/flac`),
  `ashpd` over zbus (portals), `reed-solomon-simd` (FEC), `rustix` (socket options and syscalls;
  its `net` feature needs `time` to compile), `wgpu` (GPU scaling), `rav1d` for software AV1
  decode, `rubato` for audio resampling when it is needed.
- gpui comes from git (the Zed repository), pinned to a `rev`, in `rusplay-app` only.
