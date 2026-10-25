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
- Cargo features are pure forwarding, none on by default so the default build stays pure Rust and
  needs no system library: `x264`, `x265`, `svt-av1` (each enables `rusplay-encode/<name>` through
  `rusplay-host`), `software-encoders` (all three), `pipewire` (`rusplay-capture/pipewire` and
  `rusplay-audio/pipewire`) and `flac` (`rusplay-audio/flac`). `rusplay-host` carries the same names
  and `rusplay-hostd` forwards to them. A config selecting an encoder whose feature is off fails at
  start with `BackendNotCompiled`. The encode crate has no `vaapi` or `nvenc` feature yet; they
  join this list when those backends land.
- A useful daemon today is built with
  `cargo build --release -p rusplay-hostd --features software-encoders,pipewire,flac`, which needs
  libx264, libx265, libSvtAv1Enc, PipeWire and libclang (for bindgen) on the system.
