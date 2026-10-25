---
paths:
  - "crates/rusplay-app/**"
---

# Desktop app

- The UI is gpui from the Zed repository (git dependency pinned to a `rev`), running on its
  Wayland backend only.
- The app is a thin shell over `rusplay-client` and `rusplay-host`. Session logic stays in those
  crates so `rusplay-hostd` and the app behave the same.
- Every setting in `rusplay-config` is reachable from the UI, and the UI writes the same TOML the
  headless host reads.
- The stream is presented through the same wgpu device that `rusplay-scale` uses, to avoid a copy
  between upscale and display.
