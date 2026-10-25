---
paths:
  - "crates/rusplay-input/**"
---

# Input

- `InputEvent` is the wire-neutral model: keys by evdev keycode, pointer motion (relative) and
  position (absolute, normalised to the stream), buttons, scroll, and gamepads by slot.
- The host injects through the RemoteDesktop portal and libei (`ashpd` `remote_desktop`), the
  Wayland way. No uinput, which needs root, and no X11 XTest.
- Gamepads are virtual devices on the host so games see a native controller. That piece needs
  uinput or a portal that exposes it and is open in `docs/FEATURES.md`.
- The client captures input from its gpui window, with the pointer locked in relative mode
  (`input.pointer_mode`).
- Input goes out the moment it is captured, never batched with media.
