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
  | VAAPI encode and decode | system `libva` + `libva-drm` through own bindgen bindings, generated once in `rusplay-va` and shared by both backends | `rusplay-va/libva`, enabled by `rusplay-encode/vaapi` and `rusplay-decode/vaapi` |
  | NVENC | system `libcuda.so.1` (through `rusplay-cuda`) + `libnvidia-encode.so.1`, loaded at run time through `libloading`, with own hand-written FFI for Video Codec SDK 13.0 (driver R570 or newer) | `rusplay-encode/nvenc` |
  | NVDEC | the driver's `libcuda.so.1` (through `rusplay-cuda`) and `libnvcuvid.so.1`, loaded at run time with `libloading` through own `#[repr(C)]` declarations in `src/nvdec/ffi.rs`; nothing is linked or needed at build time | `rusplay-decode/nvdec` |
  | x264 | system `libx264` through own bindgen bindings | `rusplay-encode/x264` |
  | x265 | system `libx265` through own bindgen bindings | `rusplay-encode/x265` |
  | SVT-AV1 | system `libSvtAv1Enc` through own bindgen bindings | `rusplay-encode/svt-av1` |
  | Clipboard data-control on its own connection | `wayland-client` (`system` and `dlopen`), `wayland-protocols` (`ext-data-control-v1`, staging) and `wayland-protocols-wlr` (`wlr-data-control`), the same run-time libwayland loading | `rusplay-clipboard/wayland`, enabled by `rusplay-host/input` and `rusplay-client/window` |
  | Wayland input capture on gpui's connection | `wayland-client` and `wayland-backend` (`system` and `dlopen`: libwayland-client is loaded at run time, as gpui does) | `rusplay-input/wayland` |
  | Stream subsurface on gpui's connection | the same `wayland-client` `system` and `dlopen` features | `rusplay-client/window` |
  | rav1d SIMD | rav1d's hand-written x86/ARM assembly (`nasm` at build time on x86_64, the C compiler's assembler on aarch64) | `rusplay-decode/software-asm`, `rusplay-client/software-asm` |
  | H.264 and HEVC software decode | system `libavcodec` + `libavutil` through own bindgen bindings in `crates/rusplay-decode/build.rs` | `rusplay-decode/ffmpeg`, forwarded by `rusplay-client/ffmpeg` and `rusplay-app/ffmpeg` |

  The published `x264-sys`, `x265-sys` and `svt-av1-sys` crates generate against vendored or
  ancient headers, so the software encoders generate their bindings in
  `crates/rusplay-encode/build.rs` from the headers `pkg-config` finds on the system. `bindgen`
  (with its `runtime` libclang loading) and `pkg-config` are optional build dependencies pulled in
  by the three software-encoder features and by `rusplay-decode/ffmpeg`, so the default build
  neither needs libclang nor links anything C. H.264 and HEVC software decode stays
  `video.decoder.backend = "software"`. The `ffmpeg` feature adds those codecs beside rav1d; it
  does not decode AV1 and it does not replace the `software` feature.

  VAAPI follows the same pattern in `crates/rusplay-va/build.rs`, behind that crate's `libva`
  feature, which the `vaapi` features of `rusplay-encode` and `rusplay-decode` switch on. Neither
  media crate may depend on the other, so the one set of bindings and the libva wrappers live in
  `rusplay-va`, a crate below both (`.claude/rules/driver-wrappers.md`); likewise `rusplay-cuda`
  holds the `libcuda.so.1` loading both NVIDIA backends share. `cros-libva` 0.0.12 and 0.0.13
  generate their bindings from the system headers too but do not compile against libva 2.24 (new
  `VAEncPictureParameterBufferVP9` fields), and `cros-codecs` 0.0.6 pins 0.0.12. The encode
  `build.rs` drives one `Library` description per system library (pkg-config packages, headers,
  symbol pattern, macro constant signedness) for the software encoders.
  Bitstream parsing comes from `cros-codecs` with its default features off, which is only its
  pure-Rust `codec` parsers (H.264, HEVC, AV1) and pulls in no libva, GBM or DRM crates.
  `rusplay-encode` writes its own parameter sets and headers and uses `cros-codecs` only as a
  dev-dependency, to parse them back in tests. Its other dev-dependency is `rusplay-decode` with
  `vaapi` and `software`, which decodes the hardware encoder's output in tests; that is a test-only
  edge between two media crates and never a normal dependency, but it means the encode crate's
  tests need libva and libclang even when only a software encoder feature is enabled.

  The NVIDIA libraries are never linked at build time: no SDK headers, CUDA toolkit or driver is
  needed to build or test `nvenc`, which is why it is safe under `--all-features` on any machine.
  The published `nvidia-video-codec-sdk` crate links `libcuda` and `libnvidia-encode` and needs the
  SDK at build time, so it is not used. `src/nvenc/ffi.rs` declares only the CUDA driver API and
  NVENC subset the backend calls, as `#[repr(C)]` structs whose sizes, field offsets and
  `NVENCAPI_STRUCT_VERSION` values, `NV_ENC_CAPS` positions, buffer formats and bit-field
  positions are unit tested against the 13.0 header (FFmpeg's `nv-codec-headers` `n13.0.19.1`,
  `include/ffnvcodec/nvEncodeAPI.h`). `libloading` (pure Rust over `dlopen`, used by `rusplay-cuda`) is the loader; a missing
  library is `CudaError::LibraryNotFound`, a missing symbol `CudaError::SymbolMissing`. Moving to a newer SDK means
  updating those structs and their tests together.

  `fuzz/` is the one exception outside the table: `libfuzzer-sys` compiles LLVM's C++ libFuzzer,
  which cargo-fuzz requires, and lives only in that crate (own `[workspace]`, never a dependency
  of anything under `crates/`).

  Pure-Rust choices already made: `wayland-client` and `wayland-protocols` (`client`, `staging`)
  for the compositor's output colour description in `rusplay-client`, on wayland-rs's pure-Rust
  backend, `opus-rs` (Opus), `flacenc` (FLAC, `rusplay-audio/flac`),
  `ashpd` 0.14 over zbus 6 (portals, the `async-io` feature, which is zbus's own `default-rt` runtime; `rusplay-priority` talks to rtkit with the same `zbus`, `default-rt` and `proxy` only, so rusplay's own crates share one D-Bus stack. gpui still builds ashpd 0.13 on zbus 5, so the app also carries zbus 5 until Zed moves; `rusplay-hostd` holds zbus 6 alone), `reed-solomon-simd` (FEC), `rustix` (socket options and syscalls;
  its `net` feature needs `time` to compile), `reis` (the EI protocol for libei input injection,
  `rusplay-input/libei`, with `rustix` for its socket polling and clock), `evdev` (uinput virtual
  gamepads on the host and physical gamepads with force feedback on the client,
  `rusplay-input/gamepad`; it only declares the kernel structs through `libc`; the DualSense
  personality writes `uhid_event`s by hand over `rustix` file calls instead of a uhid crate), `wayland-protocols`
  (pointer constraints, relative pointer and, from `staging`, cursor shape bindings, `rusplay-input/wayland`), `wgpu`
  (GPU scaling), `ash` (raw Vulkan for DMA-BUF import in `rusplay-scale`, default features off so
  it loads nothing itself; kept at the version wgpu-hal uses, since handles cross between them),
  `rav1d` for software AV1 decode (default features off; its `asm` feature is only enabled by
  `software-asm`, which needs `nasm` on x86_64), `cros-codecs` parsers for VAAPI decode, `rubato` for audio resampling when it is
  needed. `half` (`rusplay-client/window`, the cursor's `rgba16float` upload) and `pollster`
  (`rusplay-client/window`, blocking on wgpu's adapter and device requests). Pairing and transport encryption use RustCrypto only: `spake2` (pinned to 0.4.0, the
  latest stable release; 0.5 is still a pre-release), `hkdf`, `sha2`, `chacha20poly1305`,
  `subtle` and `zeroize`. `libc` is allowed as bare syscall declarations only for what rustix
  lacks (`recvmmsg`); it links nothing std does not already.
- **`vendor/rav1d` is rav1d 1.1.0 with three local fixes**, pointed at by `[patch.crates-io]` in
  the root manifest and in `fuzz/Cargo.toml`, and listed in the root `[workspace] exclude` so it
  is not a member. It exists because rav1d 1.1.0 (the latest release, with no upstream fix) aborts
  or leaks on hostile or ordinary input that the software decoder meets, and the fixes are small
  enough to carry. The import is its own commit (`chore: vendor rav1d 1.1.0 unchanged`), so
  `git diff` from it shows exactly what changed. The changes:
  - `src/decode.rs`, `rav1d_submit_frame::on_error`: reads `frame_hdr` with `is_some_and`
    instead of `unwrap`, because `rav1d_decode_frame_exit` has already cleared it when tile
    decoding failed. A failing frame was a panic inside `dav1d_send_data` (an abort in release
    builds); it is now an error from the send.
  - `src/cdef.rs`, `padding`: the `top` and `bottom` windows start at `x_start` (length
    `x_end - x_start`, indexed with `x - x_start`) instead of two samples left of the block.
    The first block of a row never uses those two samples, but the `DisjointMut` guard covered
    them and they belong to a line another superblock row's CDEF task writes, so debug builds
    with many worker threads tripped the overlap check (a false positive, not a data race).
  - `src/lib.rs`, `dav1d_picture_unref`: writes `Dav1dPicture::default()` back instead of
    converting a default `Rav1dPicture`, whose conversion put one raw `Arc` (the empty
    `itut_t35` list) into the struct that nothing released, leaking it for every picture.
  - `rust-toolchain.toml` is deleted, since rustup would install its pinned old nightly from
    inside the directory.
  `vendor/` is excluded from `scripts/check-comments.sh` and from the `rust-edit.sh` hook, and
  `rust-formatter` does not reach it (the manifest is not a workspace member), so the vendored
  source keeps upstream's comments and style. The vendored copy and its `[patch]` entries go
  away once an upstream release carries these fixes; update the `rav1d` version in
  `[workspace.dependencies]` then, and check each fix against the release first.
- **`vendor/cros-codecs` is cros-codecs 0.0.6 with one local fix**, set up exactly like
  `vendor/rav1d` (`[patch.crates-io]` in both manifests, `[workspace] exclude`, outside the style
  checks; the import is `chore: vendor cros-codecs 0.0.6 unchanged`). 0.0.6 is the latest release
  and its HEVC slice header kept `entry_point_offset_minus1` in a `[u32; 32]`, indexed with the
  count read from the stream, so a slice with more than 32 entry points would panic. Wavefront
  parallel processing gives one per CTU row after the first, which x265 does by default: 44 for
  1440p at 32-pixel CTUs, 67 for 2160p. The change, in `src/codec/h265/parser.rs`:
  `entry_point_offset_minus1` is a `Vec<u32>` filled with one push per offset, and
  `Slice::replace_header` (dependent slice segments) takes it with `mem::take` and also keeps the
  segment's own `num_entry_point_offsets` instead of the independent segment's. The copy and its
  `[patch]` entries go away once an upstream release fixes the array.
- gpui comes from git (the Zed repository), pinned to a `rev`, in `rusplay-app` only.
  - Its Linux backend links C libraries through `xkbcommon` (system libxkbcommon) and
    `zed-font-kit` (`freetype-sys`, `yeslogic-fontconfig-sys`). That is accepted for gpui and
    confined to `rusplay-app`; no other crate may depend on them. It also dlopens
    `libwayland-client` at runtime.
  - `gpui` and `gpui_platform` share one `rev`, both with `default-features = false`. Only the
    Wayland backend is enabled; X11 stays off.
  - Zed's workspace `[patch.crates-io]` entries that gpui needs are copied into the root
    `Cargo.toml` (`async-task`, `calloop`, `khronos-egl`).
  - The workspace `wgpu` is the git rev gpui's `gpui_wgpu` uses (`3b3506e`, the same rev Zed pins),
    not crates.io 30.0.1: wgpu-hal 30.0.1 `expect`s that no `SurfaceTexture` still holds a
    swapchain semaphore when the swapchain is released, which `panic = "abort"` turns into a dead
    process on a resize or teardown (gfx-rs/wgpu#10230); the rev skips such a semaphore instead.
    Being one source, the two are one crate in the app. Moving the gpui `rev` means moving the
    workspace `wgpu` rev with it, and the rev must keep `ash` 0.38. The stream still presents on
    its own subsurface and device (`ui.md`), so no handle crosses between them.
  - `raw-window-handle` (0.6, the version gpui implements) reads the `wl_display` and
    `wl_surface` pointers off gpui's `Window`.
  - `unicode-segmentation` (pure Rust) gives the text field grapheme boundaries.
