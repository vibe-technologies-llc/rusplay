---
paths:
  - "crates/rusplay-app/**"
---

# Desktop app

- The UI is gpui from the Zed repository (git dependency pinned to a `rev`), running on its
  Wayland backend only. The root `Cargo.toml` declares `gpui` and `gpui_platform` once at the
  same `rev` with `default-features = false`, and `rusplay-app` enables `gpui_platform/wayland`.
  The X11 features stay off, which keeps `x11rb` and `xim` out of the tree. The root also carries
  the `[patch.crates-io]` entries (`async-task`, `calloop`, `khronos-egl`) that Zed's own
  workspace applies; a git dependency does not inherit them, and gpui needs the forks. Moving the
  `rev` means re-reading Zed's `[patch.crates-io]` and `gpui_linux` features.
- The app is a thin shell over `rusplay-client` and `rusplay-host`. Session logic stays in those
  crates so `rusplay-hostd` and the app behave the same. The app only builds the config, constructs
  `ClientSession` and runs it on gpui's background executor, then shows the resulting error as text.
- Every setting in `rusplay-config` is reachable from the UI, and the UI writes the same TOML the
  headless host reads.
- The stream is presented through the same wgpu device that `rusplay-scale` uses, to avoid a copy
  between upscale and display. gpui currently links its own git `wgpu` 30.0.0 next to the
  workspace's crates.io 30.0.1, so `app-stream-view` has to reconcile the two before a device can
  be shared.

## Structure

| Module | Role |
|---|---|
| `app` | Root view: page content, status bar, global focus and quit actions |
| `hosts` | `HostList` model (address parsing, selection, no UI types) and `HostsView` |
| `session` | Loads the client config and runs `ClientSession`; the only place the app touches the session |
| `notice` | `Notice` and the `NoticeRaised` event views emit; the root renders the latest one in the status bar |
| `widgets` | `TextField` (single-line input with IME, selection, clipboard) and stateless controls |
| `theme` | The colour palette, one function per colour |

- Views never write to the status bar directly: they emit `NoticeRaised` and the root subscribes.
- The client config lives at `$XDG_CONFIG_HOME/rusplay/client.toml` (`$HOME/.config` when unset),
  resolved by `ConfigEnvironment` in `rusplay-config`. It is read again on every connect so a save
  from the settings page takes effect without a restart. A missing file means defaults; a malformed
  one is reported in the status bar and defaults are used.
- Host entries live only in memory until discovery lands.
- gpui's own transitive dependencies are C-backed on Linux (`xkbcommon`, `freetype` and
  `fontconfig` through `zed-font-kit`); that is accepted for gpui, see `dependencies.md`.
