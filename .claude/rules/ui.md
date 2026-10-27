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
| `hosts` | `HostList` model (address parsing, selection, remembered pairings, discovery merge, no UI types) and `HostsView`, which scans and emits `ConnectRequested` |
| `session` | Loads the client config, attaches the `StreamWindow` and connects; the only place the app calls into `ClientSession` |
| `stream` | `StreamView`: the stream page, owning the `ClientStream` and `StreamWindow` from connect to teardown |
| `stats` | `TimingSink` (wraps the window's sink and records `PresentedFrame` timings), `StatsSampler` and `StreamRates`; no gpui types |
| `notice` | `Notice` and the `NoticeRaised` event views emit; the root renders the latest one in the status bar |
| `widgets` | `TextField` (single-line input with IME, selection, clipboard) and stateless controls |
| `theme` | The colour palette, one function per colour |

- Views never write to the status bar directly: they emit `NoticeRaised` and the root subscribes.
- The client config lives at `$XDG_CONFIG_HOME/rusplay/client.toml` (`$HOME/.config` when unset),
  resolved by `ConfigEnvironment` in `rusplay-config`. It is read again on every connect so a save
  from the settings page takes effect without a restart. A missing file means defaults; a malformed
  one is reported in the status bar and defaults are used.
- Discovery: `HostsView::refresh` runs `session::discover` on gpui's background executor (the
  scan blocks for `network.discovery_scan_ms`) when the app starts, on each switch to the Hosts
  page and from the Refresh button, one scan at a time. The options come from the client config
  re-read for that scan (`network.discovery_port`, `discovery_group`). `HostList::merge_discovered`
  matches announces to entries by address first, then by a unique display name for a saved entry
  that was not seen at its address (the host moved), keeps a saved entry's own name, and adds the
  rest as `HostOrigin::Discovered` entries that disappear again when a later scan misses them.
  Pairing turns a discovered entry into an added one. Announces carry no host id, so the name
  match is a hint; pairing authenticates the host.
- Saved hosts live in the client host book (`rusplay_client::HostBook`, `client.md`), opened once
  at startup from the config's `peers` section; a change to `peers.client_file` applies at the
  next start. `HostList` maps between entries and `SavedHost`s: `from_saved` at startup and
  `saved_hosts` (the `HostOrigin::Added` entries) after every change, and `HostsView::persist`
  saves the book after each add, remove, rename, address change, forget and pairing, and after a
  scan that moved a saved host. A failed save is a status-bar notice and the in-memory list stays
  usable. If the book cannot be opened (unreadable or corrupt) the app starts with an empty list
  and no book, says so in the status bar, and never overwrites the file.
- Per-host client overrides (`HostProfile`, `client.md`) belong to the saved host: the Hosts page
  shows, for the selected host, one row per overridable option (`hosts/profile.rs`, no gpui
  types) with a `Global` button plus one per value, the effective value and whether it comes from
  the override or the global `client.toml`. Choosing a value or `Global` changes that one field;
  `Reset all to global` clears the profile. `HostsView` keeps the client config it last read
  (at start and on every refresh) as the global side, rejects a choice whose merged config fails
  `Config::validate` with a notice, and saves the book on every change. Setting a profile on a
  discovered host saves the host. At connect the root merges the selected host's profile over the
  freshly read `client.toml` before the window is attached, so `video.present_mode` takes
  effect, and a merge that fails validation aborts the connect with a notice
  (`SessionError::HostProfile`).
- A host without a remembered pairing needs the PIN the host shows; a successful PIN connect
  hands back `ClientStream::paired_secret`, which `StreamView` turns into a `PeerRecord` (named
  after the host entry) and the root stores on the entry, so later connects use
  `Credentials::Peer`, also after a restart. `Rename`, `Change address` (both read the Name and
  Address fields; the pairing is kept, it is keyed by the secret and not the address) and
  `Forget pairing` act on the selected entry; renaming or re-addressing a discovered entry saves
  it. Names follow `MAX_PEER_NAME_BYTES` and carry no control characters, since they are written
  to the book and become the `PeerRecord` name.

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
  header shows which, naming the chord (`chord_label`), and a pointer button next to
  `Disconnect` names the active pointer (`Pointer: auto (relative)`) and cycles
  `input.pointer_mode` for the session on click. Losing window activation
  (`observe_window_activation`) releases. `Quit` and `F11` are ignored while input is engaged,
  because the keyboard also reaches gpui and `ctrl-q` and `F11` belong to the game then; the
  client hotkeys (`input.md`) are the way to act on the client from there.
- Fullscreen: the header's `Fullscreen` button, `F11` while not engaged (root action
  `ToggleFullscreen`) and the fullscreen hotkey all call gpui's `Window::toggle_fullscreen`. The
  stream view omits its header and the root omits the status bar while the window is fullscreen
  and a stream exists, so notices are invisible then; the stats toggle and the disconnect hotkey
  are the controls that remain. A fullscreen switch changes no focus: engagement lives in the
  capture's `CaptureCore`, the pointer lock and shortcuts inhibitor are bound to the surface, and
  the layout follows through the canvas prepaint's `set_region`. Engagement is dropped only if the
  compositor itself moves keyboard focus off the surface (`wl_keyboard.leave`, an inhibitor
  `inactive` event, or gpui's activation observer), which an `xdg_toplevel.set_fullscreen` does
  not do on the compositors this was written against; it has not been exercised on a live
  compositor from this repository.
- A 50 ms poll on the foreground executor (`spawn_in`, so it can reach the `Window`) runs the
  client actions queued by the hotkeys, ends the session when any client stage, the window's
  presenter or a layout update failed, and re-renders when a newer stream description arrives so
  the letterbox follows the stream's aspect, and when the engaged state changes. Teardown stops the stream first, then the window, and
  emits `StreamEvent::Ended` with the notice for the status bar.
- Statistics: the same poll feeds `StatsSampler` with `ClientStream::stats()` and the
  `FrameTimings` that `TimingSink` fills from each `PresentedFrame`. Every 500 ms it turns the
  counter deltas into per-second rates and the timing deltas into average and peak client latency
  (`received_at` to `submitted_at`, split at `decoded_at`), and the header shows them as a muted
  line under the state, which the `Hide stats`/`Show stats` button and the stats hotkey toggle. Occasional counters (superseded, keyframe requests, decode errors, audio
  underruns) appear only while non-zero. The figures are client side only: the end-to-end time
  waits on `stream-capture-timestamps`.

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
- The page edits one config role at a time, chosen by the `Editing` tabs above the fields
  (`settings/role.rs`): `client.toml`, which the app itself loads, or `host.toml`, which
  `rusplay-hostd` discovers. Each role has its own baseline: the first visit to the host tab reads
  `host.toml` (defaults when it is missing, an error notice and no switch when it is malformed),
  never the client values. The inactive role's form, saved config and unsaved flag are parked in
  `SettingsView::parked`, so switching keeps unsaved edits and the tab names which role has them.
  `Save` and `Revert` act on the active role only. Both go through `Config::save` (validate, then
  write a sibling temporary file and rename it over the target). The client config is read again
  at connect, so a client save takes effect without a restart; a host save does only once the
  daemon restarts.
- gpui's own transitive dependencies are C-backed on Linux (`xkbcommon`, `freetype` and
  `fontconfig` through `zed-font-kit`); that is accepted for gpui, see `dependencies.md`.
