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
  | PipeWire audio capture and playback | `pipewire` | `rusplay-audio/pipewire` |
  | VAAPI encode/decode | `cros-libva` | `rusplay-encode/vaapi`, `rusplay-decode/vaapi` |
  | NVENC / NVDEC | `nvidia-video-codec-sdk` | `rusplay-encode/nvenc`, `rusplay-decode/nvdec` |
  | x264 | system `libx264` through own bindgen bindings | `rusplay-encode/x264` |
  | x265 | system `libx265` through own bindgen bindings | `rusplay-encode/x265` |
  | SVT-AV1 | system `libSvtAv1Enc` through own bindgen bindings | `rusplay-encode/svt-av1` |

  The published `x264-sys`, `x265-sys` and `svt-av1-sys` crates generate against vendored or
  ancient headers, so the software encoders generate their bindings in
  `crates/rusplay-encode/build.rs` from the headers `pkg-config` finds on the system. `bindgen`
  (with its `runtime` libclang loading) and `pkg-config` are optional build dependencies pulled in
  only by the three software-encoder features, so the default build neither needs libclang nor
  links anything C.

  Pure-Rust choices already made: `opus-rs` (Opus), `flacenc` (FLAC, `rusplay-audio/flac`),
  `ashpd` over zbus (portals), `wgpu` (GPU scaling), `rav1d` for software AV1 decode, `rubato`
  for audio resampling when it is needed.
- gpui comes from git (the Zed repository), pinned to a `rev`, in `rusplay-app` only.
