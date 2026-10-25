---
name: add-crate
description: Create a new crates/rusplay-<name> workspace member wired into the project's conventions. Use when a subsystem needs its own crate.
---

# Add a crate

1. Create `crates/rusplay-<name>/Cargo.toml` with `version`, `edition`, `license` and `publish`
   all `.workspace = true`, plus `[lints] workspace = true`. No `rust-version`.
2. Add `rusplay-<name>.path = "crates/rusplay-<name>"` to the root `[workspace.dependencies]`.
   `members = ["crates/*"]` picks the crate up automatically.
3. Give it a `src/error.rs` with its own error enum (`.claude/rules/errors.md`), re-exported from
   `lib.rs`.
4. Keep the dependency direction in `CLAUDE.md`: no upward edges.
5. Add a row to the crate table in `CLAUDE.md`, and a `.claude/rules/<subject>.md` with
   `paths: ["crates/rusplay-<name>/**"]` holding its design, plus a row in the rules table.
6. Run the `check` skill.
