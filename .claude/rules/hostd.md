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
- The binary has no cargo features of its own yet. Reaching the x264, x265 and SVT-AV1 encoders
  from it needs the features forwarded through `rusplay-host`.
