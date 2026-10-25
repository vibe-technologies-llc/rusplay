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
  | VAAPI encode | `cros-libva` | `rusplay-encode/vaapi` |
  | VAAPI decode | system `libva` + `libva-drm` through own bindgen bindings | `rusplay-decode/vaapi` |
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

  VAAPI decode follows the same pattern (`crates/rusplay-decode/build.rs`, `vaapi` feature):
  `cros-libva` 0.0.12 and 0.0.13 generate their bindings from the system headers too but do not
  compile against libva 2.24 (new `VAEncPictureParameterBufferVP9` fields), and `cros-codecs`
  0.0.6 pins 0.0.12. Bitstream parsing comes from `cros-codecs` with its default features off,
  which is only its pure-Rust `codec` parsers (H.264, HEVC, AV1) and pulls in no libva, GBM or
  DRM crates.

  Pure-Rust choices already made: `opus-rs` (Opus), `flacenc` (FLAC, `rusplay-audio/flac`),
  `ashpd` over zbus (portals), `reed-solomon-simd` (FEC), `rustix` (socket options and syscalls;
  its `net` feature needs `time` to compile), `wgpu` (GPU scaling), `rav1d` for software AV1
  decode (default features off: its `asm` feature needs `nasm`), `cros-codecs` parsers for VAAPI
  decode, `rubato` for audio resampling when it is needed. Pairing and transport encryption use
  RustCrypto only: `spake2` (pinned to 0.4.0, the latest stable release; 0.5 is still a
  pre-release), `hkdf`, `sha2`, `chacha20poly1305`, `subtle` and `zeroize`. `libc` is allowed as
  bare syscall declarations only for what rustix lacks (`recvmmsg`); it links nothing std does
  not already.
- gpui comes from git (the Zed repository), pinned to a `rev`, in `rusplay-app` only.
