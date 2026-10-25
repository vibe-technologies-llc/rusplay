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
  `ashpd` over zbus (portals), `reed-solomon-simd` (FEC), `rustix` (socket options and syscalls;
  its `net` feature needs `time` to compile), `reis` (the EI protocol for libei input injection,
  `rusplay-input/libei`, with `rustix` for its socket polling and clock), `evdev` (uinput virtual
  gamepads, `rusplay-input/gamepad`; it only declares the kernel structs through `libc`), `wgpu`
  (GPU scaling), `rav1d` for software AV1 decode, `rubato` for audio resampling when it is needed.
  Pairing and transport encryption use RustCrypto only: `spake2` (pinned to 0.4.0, the latest
  stable release; 0.5 is still a pre-release), `hkdf`, `sha2`, `chacha20poly1305`, `subtle` and
  `zeroize`. `libc` is allowed as bare syscall declarations only for what rustix lacks
  (`recvmmsg`); it links nothing std does not already.
- gpui comes from git (the Zed repository), pinned to a `rev`, in `rusplay-app` only.
  - Its Linux backend links C libraries through `xkbcommon` (system libxkbcommon) and
    `zed-font-kit` (`freetype-sys`, `yeslogic-fontconfig-sys`). That is accepted for gpui and
    confined to `rusplay-app`; no other crate may depend on them. It also dlopens
    `libwayland-client` at runtime.
  - `gpui` and `gpui_platform` share one `rev`, both with `default-features = false`. Only the
    Wayland backend is enabled; X11 stays off.
  - Zed's workspace `[patch.crates-io]` entries that gpui needs are copied into the root
    `Cargo.toml` (`async-task`, `calloop`, `khronos-egl`).
  - gpui's `gpui_wgpu` uses Zed's own git `wgpu`, not the crates.io `wgpu` the workspace declares,
    so the two are separate crates in the tree.
  - `unicode-segmentation` (pure Rust) gives the text field grapheme boundaries.
