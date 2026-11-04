---
paths:
  - "crates/rusplay-clipboard/**"
  - "crates/rusplay-host/src/clipboard.rs"
  - "crates/rusplay-client/src/clipboard.rs"
  - "crates/rusplay-protocol/src/clipboard.rs"
---

# Clipboard sync

- Text only, `text/plain;charset=utf-8`, between the client and the host. It is user data crossing
  machines, so it is off by default: `input.clipboard` (`off`, `client-to-host`, `host-to-client`,
  `both`) and `input.clipboard_max_bytes` (default 65536, range 1 to 1048576). Each end applies its
  own copy of the setting. `ClipboardPolicy::host` and `::client` turn the direction into `send`
  and `receive` flags (`ClipboardDirection::host_sends` and `client_sends`): a side that may not
  receive never reassembles or applies a message, and a side that may not send never reads its
  local clipboard (`DataControlOptions::watch`, `PortalClipboard::link` with `watch` false,
  `ClipboardLink::service` skipping `poll_change`).
- Clipboard messages travel on the reliable control channel as `ControlMessage::Clipboard`
  (`networking.md`), never on a media stream. `ClipboardSync` is the pure engine (no I/O, unit
  tested): `local_changed` gates by direction, rejects empty and oversized text and text equal to
  `last_synced`, and queues the chunks of a new transfer, replacing any unsent older ones;
  `receive` reassembles through `ClipboardReassembler` and returns text to apply unless it equals
  `last_synced`. `last_synced` is set by both a send and an apply, which is the whole echo
  suppression: a value applied from the peer reads back as a local change and is dropped, and a
  value sent is not sent again when the peer echoes it. It is content based on purpose, because a
  compositor may or may not report one's own selection back.
- `ClipboardLink` pairs a `ClipboardSync` with a `Box<dyn ClipboardBackend>` and lives inside the
  host control stage and the client receive stage, so no thread of its own touches the control
  channel and nothing on the frame path waits for it. `service` is called once per loop pass (at
  most 10 to 20 ms apart): it drains backend changes into the engine, then offers queued chunks
  to a `send` closure that returns false when the control window is full, which leaves the rest
  queued for the next pass. A chunk the control channel gives up on is only counted
  (`control_undeliverable`); the receiver keeps the unfinished transfer until a newer one
  replaces it, so a lost chunk costs one clipboard update, never a stall.
- A protocol error while receiving (invalid UTF-8, an over-limit or inconsistent transfer) is
  counted in `malformed_datagrams` and the text is dropped. A backend error drops the link: the
  session keeps streaming without clipboard.
- Clipboard contents are never logged. `ClipboardChunk` has a hand-written `Debug` that omits the
  bytes, and no error variant carries text.
- `ClipboardState` (`Off`, `Active(BackendKind)`, `Unavailable(Arc<ClipboardError>)`) is what a
  session reports when it opens its link: `HostStream::clipboard`, `ClientStream::clipboard`,
  `HostEvent::ClipboardUnavailable` (printed by `rusplay-hostd`, a status notice in the app).
  A compositor with no backend is `Unavailable`, never a failed session.

## Backends

- `ClipboardBackend` is `kind`, `poll_change` (non-blocking, `Ok(None)` when nothing changed) and
  `set_text`. `MemoryClipboard` is the in-process one tests use; `HostSession::with_clipboard` and
  `ClientSession::with_clipboard` take a `ClipboardOpener` (`Fn(ClipboardPolicy) -> Result<Box<dyn
  ClipboardBackend>>`) that is called when a session starts. Without one, a host built with
  `input` and a client built with `window` open data-control; a build without them reports
  `NotCompiled`.
- `data_control` (feature `wayland`, pure-Rust `wayland-client` with `dlopen`/`system`, like the
  input crate) speaks `ext-data-control-v1` and falls back to `zwlr_data_control_manager_v1`. One
  macro (`flavour!`) instantiates the same code for both protocols. It opens its own connection
  and thread (`rusplay-clipboard`), binds the first seat, and polls the socket together with the
  pipes of pending reads every 10 ms. A `selection` event is read only when the offer lists
  `text/plain;charset=utf-8` and not `x-kde-passwordManagerHint` (password managers set it), into
  a non-blocking pipe (`ReadJob`) capped at `max_bytes` and 2 s; an oversized, stalled or invalid
  UTF-8 selection is dropped. The selection already in place when the connection opens is never
  reported (events before the first roundtrip ends are ignored). `set_text` creates a data source
  offering only that mime type; each `send` request is served from a short thread writing with a
  2 s timeout, so a stalled reader never blocks the loop. The primary selection is ignored.
- `portal` (feature `portal`) is the Clipboard portal on the RemoteDesktop session the host
  already opens. `request_clipboard` is called on that session before `Start`, in the same
  dialog, only when clipboard sync is on and `probe_data_control` finds no data-control global
  (so a compositor with data-control never asks for the extra permission). The session is shared
  as `Arc<RemoteDesktopSession>` between the injector and `PortalClipboard`, whose two threads
  listen for `SelectionOwnerChanged` and `SelectionTransfer` and issue `SetSelection`;
  `PortalClipboard::link` hands each client session its own backend over the one portal session.
  A failure to request or start it is `ClipboardError::PortalSetup`, reported as unavailable.
- Host choice: data-control when the compositor has it (KWin: `ext-data-control-v1`, wlroots:
  either), else the portal clipboard, else unavailable (plain GNOME without the portal's
  clipboard). Client choice: data-control only. gpui has no clipboard change notification, so a
  client on a compositor without data-control (GNOME) cannot watch its clipboard and reports
  unavailable.

## Tests

- Pure logic (wire, limits, echo, direction gating, `ReadJob`) has unit tests. The host and client
  crates run the real control stages against `MemoryClipboard`, and `rusplay-host/tests/loopback.rs`
  runs a real `HostSession` and `ClientSession` over loopback with one `MemoryClipboard` each.
- `rusplay-clipboard/tests/nested.rs` runs the data-control backend against a nested, headless
  `kwin_wayland --virtual` started under `dbus-run-session` on its own socket, so it never touches
  the user's clipboard or compositor. It skips visibly (`skipped <test>: ...`) when
  `dbus-run-session` or `kwin_wayland` is missing, and fails under `RUSPLAY_REQUIRE_HW=1`.
  The wlr flavour and the portal backend have no live test: the nested KWin offers only
  `ext-data-control-v1`, and the portal one needs a person to confirm a dialog.
