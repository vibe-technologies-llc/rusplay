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
  crates so `rusplay-hostd` and the app behave the same. The app builds the config, attaches a
  `rusplay_client::StreamWindow` to its own window, runs `ClientSession::connect` with that
  window's `Gpu` and `WindowSink` on gpui's background executor, and shows any error as text.
- Every setting in `rusplay-config` is reachable from the UI, and the UI writes the same TOML the
  headless host reads.
- The stream never goes through gpui's renderer: on Linux gpui exposes neither its wgpu device nor
  a way to draw a foreign texture (`paint_surface` is macOS only). `StreamWindow` (`rusplay-client`, `window`
  feature, `client.md`) puts a Wayland subsurface over the picture rectangle and presents into it
  with its own wgpu device, the same one the client's present stage and `rusplay-scale` use, so
  the scaled texture is composed straight into the swapchain with no copy and no wait on gpui's
  frame loop. gpui keeps its own Zed git `wgpu` 30.0.0 next to the workspace's crates.io 30.0.1;
  nothing crosses between the two, so they stay separate crates.
- The app gets the `wl_display` and `wl_surface` pointers from gpui's `Window` through
  `raw-window-handle` (`HasWindowHandle::window_handle` explicitly, since `Window` has an inherent
  method of the same name). Their validity is the one `unsafe` contract: the `StreamWindow` lives
  in the `StreamView` of the same window, and the root ends the stream on `Quit` and in
  `on_window_should_close`, before gpui destroys the surface.
- Features: `pipewire` and `flac` are on by default (audio playback and FLAC); the decoders are
  opt-in like the host's encoders: `vaapi`, `software`, `software-asm` forward to
  `rusplay-client`. A build that can stream is
  `cargo build --release -p rusplay-app --features vaapi,software`.

## Structure

| Module | Role |
|---|---|
| `app` | Root view: page content, status bar, global focus and quit actions |
| `hosts` | `HostList` model (address parsing, selection, remembered pairings, no UI types) and `HostsView`, which emits `ConnectRequested` |
| `session` | Loads the client config, attaches the `StreamWindow` and connects; the only place the app calls into `ClientSession` |
| `stream` | `StreamView`: the stream page, owning the `ClientStream` and `StreamWindow` from connect to teardown |
| `notice` | `Notice` and the `NoticeRaised` event views emit; the root renders the latest one in the status bar |
| `widgets` | `TextField` (single-line input with IME, selection, clipboard) and stateless controls |
| `theme` | The colour palette, one function per colour |

- Views never write to the status bar directly: they emit `NoticeRaised` and the root subscribes.
- The client config lives at `$XDG_CONFIG_HOME/rusplay/client.toml` (`$HOME/.config` when unset),
  resolved by `ConfigEnvironment` in `rusplay-config`. It is read again on every connect so a save
  from the settings page takes effect without a restart. A missing file means defaults; a malformed
  one is reported in the status bar and defaults are used.
- Host entries live only in memory until discovery lands. A host without a remembered pairing
  needs the PIN the host shows; a successful PIN connect hands back `ClientStream::paired_secret`,
  which `StreamView` turns into a `PeerRecord` (named after the host entry) and the root stores on
  the entry, so later connects in the same run use `Credentials::Peer`.

## Stream page

- While a stream exists the root shows only `StreamView` and the status bar: a header with the
  state and `Disconnect`, and below it a black area holding a `canvas`. The canvas's prepaint
  turns its bounds and `Window::scale_factor` into `WindowLayout::letterbox` against the
  stream description's resolution, calls `ClientStream::set_target` with the device size and
  `StreamWindow::set_layout` with the rectangle. Doing it in prepaint means the subsurface
  position (applied on the parent's commit) lands with the frame gpui is drawing.
- Input: once connected, `StreamView` calls `StreamWindow::capture_input` with the config's
  `input` section; a failure there is reported as a notice and the stream continues without
  input. A click on the picture or the release chord engages, the chord releases again; the
  header shows which, naming the chord (`chord_label`). Losing window activation
  (`observe_window_activation`) releases. `Quit` is ignored while input is engaged, because the
  keyboard also reaches gpui and `ctrl-q` belongs to the game then.
- A 50 ms poll on the foreground executor ends the session when any client stage, the window's
  presenter or a layout update failed, and re-renders when a newer stream description arrives so
  the letterbox follows the stream's aspect, and when the engaged state changes. Teardown stops the stream first, then the window, and
  emits `StreamEvent::Ended` with the notice for the status bar.

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
