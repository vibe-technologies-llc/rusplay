---
paths:
  - "crates/rusplay-hostd/**"
---

# Headless host binary

- Arguments are parsed with `lexopt` in `cli.rs`: `-c`/`--config <PATH>`, `-h`/`--help`,
  `-V`/`--version`. No positional arguments. A usage error exits with status 2 after printing the
  cause and a pointer to `--help`; any other failure exits with status 1 and prints the cause
  chain.
- Config path discovery is `discovery.rs`, pure over an `Environment` (`XDG_CONFIG_HOME`, `HOME`)
  and an `exists` predicate so tests need no filesystem. Order: `--config` (used as given, a
  missing file is a read error and not a fallthrough), `$XDG_CONFIG_HOME/rusplay/host.toml`,
  `~/.config/rusplay/host.toml`, then `Config::default()`. A relative or empty
  `XDG_CONFIG_HOME` is ignored as the XDG specification says.
- `responder.rs` runs the `DiscoveryResponder` (re-exported by `rusplay-host`) on the thread
  `rusplay-hostd-discovery`, bound on `network.discovery_port` and `network.discovery_group` and
  announcing the host's media port (the port `HostSession` actually bound) and its name, so a
  client's `DiscoveryBrowser` finds the host without an address. The name is `peers.host_name`
  when set, else the kernel hostname from `/proc/sys/kernel/hostname`, else `rusplay`, cut to 64
  bytes on a character boundary. It starts after the session binds, before the first PIN, and
  stops when the daemon returns; a bind failure (the port is taken) ends the daemon with the
  cause, a later receive error is printed and retried after a second. `rusplay-net` is a
  dev-dependency only, for the browser in its test.
- Cargo features are pure forwarding, none on by default so the default build stays pure Rust and
  needs no system library: `vaapi`, `x264`, `x265`, `svt-av1` (each enables `rusplay-encode/<name>`
  through `rusplay-host`), `software-encoders` (the three software ones), `pipewire`
  (`rusplay-capture/pipewire` and `rusplay-audio/pipewire`) and `flac` (`rusplay-audio/flac`).
  `rusplay-host` carries the same names and `rusplay-hostd` forwards to them. A config selecting an
  encoder whose feature is off fails at start with `BackendNotCompiled`. The encode crate has no
  `nvenc` feature yet; it joins this list when that backend lands.
- `main.rs` opens the paired-client store (`PeerStore::open_host`), the portal capture
  (`open_portal_capture`, so the picker shows once at start), the GPU and the `HostSession`,
  then loops: a fresh PIN from `Pin::generate(network.pairing_pin_digits)` is printed for each
  `network.pairing_timeout_ms` window, `accept` takes a PIN pairing or a remembered client's
  reconnect, and `serve(..).wait()` streams until the client leaves. An accept timeout or a
  locked PIN just issues a new PIN; an error for which `HostError::is_session_failure` holds is
  printed and the loop goes on; any other error ends the daemon.
- A useful daemon today is built with
  `cargo build --release -p rusplay-hostd --features vaapi,software-encoders,pipewire,flac`, which
  needs libva, libx264, libx265, libSvtAv1Enc, PipeWire and libclang (for bindgen) on the system.
  `vaapi,pipewire` covers the default config, whose encoder backend is VAAPI; without `pipewire`
  the daemon stops at start with `CaptureNotCompiled`.
