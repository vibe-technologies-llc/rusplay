---
paths:
  - "crates/rusplay-config/**"
---

# Configuration

- `Config` is the root, with one section per subsystem: `video`, `audio`, `network`, `input`,
  `capture`, `scaling`, `color`, `peers`. Each section is its own module and struct.
- Every struct is `#[serde(default, deny_unknown_fields)]` with a hand-written `Default` holding
  the shipped default. A missing key means the default, and a misspelt key is an error. The empty
  document parses to `Config::default()`, and a test keeps it that way.
- Choices are enums with kebab-case serde names, not strings or magic numbers. Closed sets the
  user picks from (presets, tunes, filters) are enumerated in full.
- Software encoders (x264, x265, SVT-AV1) expose their common knobs as typed fields plus a
  `params` map of raw `key = value` pairs passed straight to the library, so anything the
  library accepts can be set without a Rusplay release. Typed fields win over `params` on
  conflict.
- `validate` checks ranges that serde cannot express, each failure its own `ConfigError`
  variant. `Config::load` reads, parses and validates.
- Role files (`ConfigRole`: `client.toml`, `host.toml`) are located by one resolver,
  `ConfigEnvironment`: `candidates(role)` lists `$XDG_CONFIG_HOME/rusplay/<file>` then
  `~/.config/rusplay/<file>` (a relative or empty directory is ignored, duplicates collapse), and
  `config_path(role)` is the first candidate that exists, else the first candidate, which is where
  a save creates it. `rusplay-hostd` and the app both go through it, so they never read and write
  different files.
- `input.clipboard` (`ClipboardDirection`: `off` default, `client-to-host`, `host-to-client`,
  `both`) and `input.clipboard_max_bytes` (default 65536, `ClipboardMaxBytesOutOfRange` outside
  1 to 1048576) are the only switches for clipboard sync (`clipboard.md`); each end reads its own.
- `peers.host_on_launch` (default off) tells the desktop app to start hosting when it opens.
  `rusplay-hostd` ignores it, because that process is already the host.
- The virtual output options are `capture.virtual_max_resolution` and `virtual_max_refresh_hz`
  (upper bounds on what a client's display may ask for), `virtual_min_resolution` (default
  200x200, validated nonzero and within the maximum), `virtual_resize_min_interval_ms` (default
  250, 10 to 60000) and `virtual_first_frame_timeout_ms` (default 500, 0 to 10000, 0 turns the hold
  off). The client
  side is `video.display_debounce_ms` (default 200, up to 5000). See `host.md` and `client.md`.
- Settings that depend on the machine, the display or the source default to `auto`; files carry
  `version = 3`, a version-1 file has its old shipped defaults turned into `auto` on load and a
  file older than version 3 has the old deband thresholds retuned (`auto.md`). A stored-format change bumps `CONFIG_VERSION` with a migration and a test against
  the older shape.
- New options go through the `add-config-option` skill.
- `HostProfile` is not part of `Config`: it is the sparse per-host overlay of client options the
  client's host book stores (`client.md`). Every field is an `Option`, `overlay` replaces the
  matching `Config` values and `apply` validates the merged result with `Config::validate`, so a
  profile is never checked on its own.

Config saves, portal tokens, PIN files and peer stores share `rusplay_core::write_owner_only`: owner-only creation, exclusive sibling staging, sync and rename. Token and PIN reads reject symlinks and non-regular files.
