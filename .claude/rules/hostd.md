---
paths:
  - "crates/rusplay-hostd/**"
---

# Headless host binary

- Arguments are parsed with `lexopt` in `cli.rs`: `-c`/`--config <PATH>`, `-h`/`--help`,
  `-V`/`--version`, and the subcommands `peers list` / `peers forget <ID|NAME>`, `pin`, the
  control requests `status` / `new-pin` / `forget <ID|NAME>` and `config default` /
  `config check [PATH]` (below); any other positional word is
  `UnknownCommand`. A usage error exits with status 2 after printing the
  cause and a pointer to `--help`; any other failure exits with status 1 and prints the cause
  chain.
- `peers` (`peers.rs`) opens the paired-client store (`PeerStore::open_host`, so `peers.host_file`
  applies) from the same discovered config and exits without touching the GPU, portal or
  network. `list` prints `<id> <name>` per line (never a secret). `forget` takes a name (exact)
  or an id (case-insensitive, at least 4 characters of prefix) and refuses no match
  (`NotFound`) or several (`Ambiguous`) rather than guess. It edits the store file only; a
  running daemon notices through `PeerStore::is_stale`/`refresh` (a stat of the file's
  modified time, length and inode), which `accept` checks before every pairing or reconnect
  datagram, so a forgotten client is refused at its next reconnect attempt and the daemon never
  rewrites the file from a stale in-memory list. A client already streaming stays until it
  leaves (`forget` through the control socket behaves the same). A store file that fails to parse during a refresh
  fails the accept window (printed, then a new PIN), never silently keeps the old list.
- `config` (`config.rs`) never touches the GPU, portal, network or peer store. `config default`
  prints `Config::default()` as TOML. `config check` resolves the path like a normal start (the
  positional `PATH` or `--config`, either but not both, else discovery), runs `Config::load`
  (every section is `deny_unknown_fields`, then `validate`) and prints `<path> is valid`, or the
  cause chain with status 1, so a typo is found without starting a capture session. With no
  file found it says the defaults apply.
- Config path discovery is `discovery.rs`, pure over a `ConfigEnvironment` (`XDG_CONFIG_HOME`,
  `HOME`) and an `exists` predicate so tests need no filesystem. The candidate list is
  `ConfigEnvironment::candidates(ConfigRole::Host)` from `rusplay-config`, the one the app's Host
  settings resolve through too (`config.md`). Order: `--config` (used as given, a missing file is
  a read error and not a fallthrough), `$XDG_CONFIG_HOME/rusplay/host.toml`,
  `~/.config/rusplay/host.toml`, then `Config::default()`. A relative or empty `XDG_CONFIG_HOME`
  is ignored as the XDG specification says.
- The discovery responder is `rusplay_host::Responder` (`responder.rs` in `rusplay-host`), started by
  `HostService::start` (`host.md`): a `DiscoveryResponder` on the thread `rusplay-host-discovery`,
  bound on `network.discovery_port` and `network.discovery_group` and announcing the host's
  media port (the port `HostSession` actually bound) and its name, so a client's
  `DiscoveryBrowser` finds the host without an address. The name is `peers.host_name`
  when set, else the kernel hostname from `/proc/sys/kernel/hostname`, else `rusplay`, cut to 64
  bytes on a character boundary. It starts after the session binds, before the first PIN, and
  stops when the service stops; its announces carry the stable host id, `load_or_create_host_id` on a `host-id` file
  next to the peer store file (so the id lives and moves with the pairings), and `HostSession::busy_flag`, raised while a
  `HostStream` lives (from `serve` until the stream is dropped, not during pairing or accept),
  so a client's host list can show the host as mid-session; a bind failure (the port is taken) ends the daemon with the
  cause (`HostError::DiscoveryBind`), a later receive error is printed and retried after a second.
- Cargo features are pure forwarding, none on by default so the default build stays pure Rust and
  needs no system library: `vaapi`, `nvenc`, `x264`, `x265`, `svt-av1` (each enables
  `rusplay-encode/<name>` through `rusplay-host`), `software-encoders` (the three software ones), `pipewire`
  (`rusplay-capture/pipewire` and `rusplay-audio/pipewire`), `input` (`rusplay-input/libei` and
  `rusplay-input/gamepad`, the host-side injection) and `flac` (`rusplay-audio/flac`).
  `rusplay-host` carries the same names and `rusplay-hostd` forwards to them. A config selecting an
  encoder whose feature is off fails at start with `BackendNotCompiled`. `nvenc` links nothing at
  build time (the NVIDIA libraries are loaded when an encoder opens), so it is safe in any build.
- `HostEvent::ClipboardUnavailable` is printed as `clipboard sharing is off for this session` with
  the cause chain; the session keeps streaming (`clipboard.md`).
- `main.rs` opens the paired-client store (`PeerStore::open_host`, shared as an
  `Arc<RwLock<PeerStore>>`), opens the pin file, and hands the config to
  `HostService::start` (`host.md`), which opens the GPU (first, because the capture offers the
  DMA-BUF modifiers that device imports), the portal (the capture and, with
  `input.accept_remote`, the input injector from one session, so the permission dialog shows once
  at start), binds, starts the discovery responder and spawns the accept and serve loop. The daemon
  has no accept or serve loop of its own: `supervisor.rs` (`Supervisor::run`) drains
  `HostService::poll_events` and reads `snapshot` every 50 ms on the main thread, and ends when the
  service has finished: `HostEvent::Stopped(Some(error))` (any error for which
  `HostError::is_session_failure` does not hold) ends the daemon with that error, a session failure
  (`SessionFailed`) is printed and the service goes on. The events become the same stderr lines as
  ever: `pairing pin <pin> for the next <n> s` (`PairingOpened`), `too many wrong pins, issuing a
  new one` (`PairingLocked`), `paired and streaming to <addr>` or `streaming to <addr>`
  (`ClientAdmitted`), `<addr> left` (`ClientLeft`). The PIN is generated by the service, so
  `network.pairing_pin_digits` and `network.pairing_timeout_ms` are its settings too.
- `pin_file.rs` is the unattended pairing path. With `peers.write_pin_file` (default off) the
  supervisor writes the PIN of each `PairingOpened` to `peers.pin_file`, else `$XDG_RUNTIME_DIR/rusplay/pin` (an
  unset or relative `XDG_RUNTIME_DIR` with no `peers.pin_file` is `NoRuntimeDirectory` at start),
  next to the stderr line. The directory is created mode 0700 and the file written mode 0600 to
  a `.tmp` sibling and renamed, so a reader never sees a partial PIN; the file is removed on `ClientAdmitted`,
  `PairingLocked`, `SessionFailed` and `Stopped` (a window that times out or is renewed is
  overwritten by the next `PairingOpened`) and on exit, and a failed write is printed
  and does not stop the window. `rusplay-hostd pin` prints that file's PIN (`NoWindowOpen`
  when it is absent). `dist/systemd/rusplay-hostd.service` is a user unit
  (`PartOf=graphical-session.target`, `Restart=on-failure`) expecting the binary at
  `/usr/bin/rusplay-hostd`; the portal grant already survives restarts through
  `capture.restore_token_file`.
- `control.rs` and `control_server.rs` are the control socket. With `peers.control_socket`
  (default on) the daemon binds `peers.control_socket_path`, else
  `$XDG_RUNTIME_DIR/rusplay/control.sock`, after the session has bound its port, with the
  directory created 0700 and the socket chmod 0600. A failure to bind (including no usable
  runtime directory) is printed and the daemon runs without the socket, never stops. An existing
  path is a stale socket and is replaced only when connecting to it is refused; a live one is
  `AlreadyRunning` and a non-socket file is `NotASocket`. The thread `rusplay-hostd-control`
  polls a non-blocking listener every 50 ms and serves one connection at a time with 1 s
  read and write timeouts; the session never waits on it. The protocol is one request line
  of at most 256 bytes of plain text (`status`, `new-pin`, `forget <ID|NAME>`; larger is
  `too-long`, control characters or invalid UTF-8 `malformed`) answered by `ok` and a body or
  `err <code> [detail]` (`Refusal`), after which the server half-closes, drains up to 64 KiB for
  200 ms so the reply is not lost to a reset, and closes. `status` prints `listening`, `state`
  (`starting`, `pairing` with `window_remaining_s`, or `streaming <addr>`) and `paired_clients`,
  never a PIN or secret. `new-pin` generates the PIN in the control thread and hands it to the service through its
  `PinRenewer` (`HostService::pin_renewer`, `host.md`), which raises the cancel flag
  `HostSession::accept_cancellable` polls, so the open window ends and the next one opens with the
  requested PIN instead of a generated one (printed to stderr and the PIN file as usual); the reply
  is that PIN. The renewer takes the request and the service's streaming state under one lock, so
  a request is never lost nor cancels a window it was not answered with. While a client streams it
  is refused (`streaming`, from `HostError::StreamActive`). `status` reads a `SharedPhase` the
  supervisor sets from the events (`PairingOpened` is `pairing`, `ClientAdmitted` is
  `streaming`). `forget` resolves like `peers forget` against the daemon's own shared
  `PeerStore` (refreshed first when stale) and edits the file through it; it does not end a
  session already streaming. The CLI requests go through `control::send` (5 s timeouts, 16 KiB
  reply cap) and `NotRunning` says no daemon answers.
- `stats.rs` (`StreamLog`, one per client, created on `ClientAdmitted`) logs the session from
  `HostService::snapshot`, which the supervisor reads every 50 ms: it prints the negotiated
  format once, a one-line `HostStats` summary (RTT, frame, keyframe, datagram, loss, audio,
  cursor and input counters) to stderr every `network.stats_log_interval_ms` (default 10000, `0`
  turns the periodic line off) and, on `ClientLeft`, a final `session summary` line from
  `HostService::ended_stream`, the snapshot the service reads just before it joins the stages, even
  when the session ends in an error.
  With `network.stats_json` (default off) every periodic line and the final one are also
  written to stdout as one JSON object per line (`stats_json.rs`, written by hand, no
  `serde_json`): `{"event":"stats","phase":"interval"|"final","unix_ms":..,` then every
  `HostStats` counter under its field name, `rtt_us` for the round trip and `null` for a
  value not measured yet. The human lines stay on stderr, so stdout carries only JSON. A test
  compares the emitted keys with the `Debug` output of `HostStats`, so a new counter that is
  not added to the line fails it. The line builder (`JsonLine`), `StatsPhase`, `unix_millis_now`
  and the interval `StatsSchedule` live in `rusplay-core` (`stats_report.rs`) and reach the daemon
  through `rusplay-host`'s re-exports, shared with the client's lines (`client.md`).
- A useful daemon today is built with
  `cargo build --release -p rusplay-hostd --features vaapi,nvenc,software-encoders,pipewire,input,flac`,
  which needs libva, libx264, libx265, libSvtAv1Enc, PipeWire and libclang (for bindgen) on the
  system. `vaapi,pipewire,input` covers the default config, whose encoder backend is VAAPI;
  without `pipewire` the daemon stops at start with `CaptureNotCompiled`, and without `input` with
  `InputNotCompiled` unless `input.accept_remote` is off. Virtual gamepads need access to
  `/dev/uinput` (`input.md`).
- `tests/nested/` is the live check against a real compositor and portals without touching the
  user's session. It is the `nested` test target (`required-features = ["pipewire", "svt-av1"]`),
  every test `#[ignore]`d and serialised, skipping visibly through `src/hardware.rs` when a tool,
  the PipeWire socket or a GPU adapter is missing. `Stack` runs `dbus-run-session` (a private bus
  whose address must differ from the caller's) and a `kwin_wayland --virtual` on its own socket,
  with `XDG_*_HOME` in a temp directory and `WAYLAND_DISPLAY` and `DISPLAY` set by the harness
  for every child; the portal frontend and backend are activated on that bus, and PipeWire is the
  shared user instance. Nothing opens the user's compositor, bus, `/dev/uinput` or any config.
  The portal dialog is answered inside the nested compositor with `org_kde_kwin_fake_input` (a
  pointer move and Enter, repeated until hostd prints its PIN), `Painter` draws on the virtual
  output (an `xdg_toplevel` made fullscreen on the output that is not `Virtual-0`) so it has
  damage, and the client is `rusplay-client` with the software AV1 decoder against SVT-AV1 on
  loopback. `offer.rs` holds the KWin findings of `capture.md`, `service.rs` the pairing, `pin`,
  `new-pin`, `status`, `peers list`, `forget`, stats line and reconnect run and a restart with the
  restore token. Run them with
  `cargo test -p rusplay-hostd --features pipewire,svt-av1 --test nested -- --ignored --test-threads=1`.
  `rusplay-client`, `wayland-client` and the Plasma and xdg protocol bindings are dev-dependencies
  of the daemon for these tests only. They show a nested virtual backend over loopback; none of it
  is a second machine or a real display (`hostd-service-live-check`).
