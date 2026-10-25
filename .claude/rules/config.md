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
- New options go through the `add-config-option` skill.
