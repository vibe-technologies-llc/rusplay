---
name: add-dependency
description: Add a third-party crate to the Rusplay workspace with the right version, features and placement. Use whenever a new dependency is needed.
---

# Add a dependency

1. Look for a pure-Rust crate first. A C-backed crate needs a row in the allowlist in
   `.claude/rules/dependencies.md` and a cargo feature in the crate that uses it.
2. Look up the latest version now: `cargo search --limit 1 <crate>` and `cargo info <crate>` for
   its features. Never use a remembered version.
3. Add it to the root `[workspace.dependencies]` with the full three-part version, and with
   `default-features = false` if the defaults bring in anything unused.
4. In the member crate: `<crate> = { workspace = true, features = [...] }`, listing only the
   features the code uses. Optional dependencies use `optional = true` and a `dep:` feature.
5. `rust-formatter` (it also sorts manifests), then the `check` skill.
