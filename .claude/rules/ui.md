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
  `ClientSession` and calls `probe` on gpui's background executor (until `app-stream-view` calls
  `connect` with the shared device and a `FrameSink`), then shows the resulting error as text.
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

## Settings

- The settings page is data-driven. `settings/schema.rs` is one static `FIELDS` table with a row
  per option: dotted TOML path, label, unit, `Kind` (flag, bounded integer, decimal, choice,
  multi-choice, text, IP address, resolution, frame rate, key/value pairs), whether it is
  optional, and `when` conditions. `settings/form.rs` (`FormModel`, no gpui types) holds one
  `Value` per row, converts a `Config` into rows and rows back into a TOML table that is
  deserialized into `Config` and then `validate()`d. `settings/view.rs` renders rows by `Kind`:
  toggles for flags, segmented buttons for up to four choices, dropdowns beyond that (optional
  choices gain a `(none)` entry), chips for multi-choices, `TextField` for numbers and text, and
  an editable list for `params`.
- A new `rusplay-config` option needs a row in `FIELDS`; tests fail until it has one:
  `every_serialized_option_has_a_form_field` walks populated configs for every encoder and decoder
  backend and `every_choice_option_is_accepted_by_the_config` feeds each listed choice through
  serde. Choice strings are the serde names, never display labels.
- `when` only gates the tagged-enum selectors (`video.encoder.backend`,
  `video.encoder.rate_control.mode` and `video.decoder.backend`), because those decide which keys
  exist at all and the structs are `deny_unknown_fields`. Everything else is always shown and
  always written, so a value that does not apply to the current codec is never lost on save. A
  test pins this.
- Switching backend or rate-control mode reveals rows that were absent from the loaded file; their
  initial values come from the config crate's defaults (`RateControl::DEFAULT_VBR`,
  `DEFAULT_CONSTANT_QUALITY` and each encoder and decoder backend's `Default`), never from the UI.
- Errors are per field. Typed parsing reports on the edited field as the user types; `Save` runs
  every visible row, collects all type errors, and only when there are none runs
  `Config::validate`, whose `ConfigError::field()` names the row to mark. A rejected save jumps to
  the section of the first marked row.
- `Save` writes `client.toml`; `Save as host config` writes `host.toml` from the same form, which
  is the file `rusplay-hostd` discovers. Both go through `Config::save` (validate, then write a
  sibling temporary file and rename it over the target).
- gpui's own transitive dependencies are C-backed on Linux (`xkbcommon`, `freetype` and
  `fontconfig` through `zed-font-kit`); that is accepted for gpui, see `dependencies.md`.
