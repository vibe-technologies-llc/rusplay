# Code style

- **No comments, in any language.** No `//`, `///`, `//!` or `/* */` in Rust, none in WGSL
  shaders, none in TOML manifests. What a comment would say becomes a name, a type or an extracted
  function; design rationale goes into these rules files or `docs/`. When touching a file that
  has a comment, delete it. Checked by `scripts/check-comments.sh` and the `PostToolUse` hook
  `.claude/hooks/rust-edit.sh`, which rejects an edit that adds one.
- **Paragraphs by kind.** Group bindings together, then assertions, then the next bindings, with
  a blank line between groups. Binding the subject of a test is its own paragraph. A preference,
  not a law, when following the data flow reads better.
- **Imports**: std/core/alloc, then external crates, then `crate`/`super`/`self`, one merged
  `use` per crate. `rust-formatter` enforces this and the hook runs it on every edited `.rs`
  file. It needs nightly rustfmt, which is why the repo has no `rust-toolchain.toml` pin: a pin
  makes rust-formatter use that toolchain's rustfmt.
- **Edition 2024 idioms, no MSRV.** No `rust-version` in any manifest.
- **Synchronisation uses `parking_lot`**: `Mutex`, `RwLock`, `Condvar`, `Once`. Enforced by
  `disallowed-types` in `clippy.toml`.
- **`ahash`** may replace the hasher of a map or set whose keys are not attacker controlled.
  Anything keyed by network data keeps the std hasher. Never introduce a hash map just to use
  it.
- **Clippy pedantic is on** at warn level (deny in `check`). `missing_errors_doc`,
  `missing_panics_doc` and `must_use_candidate` are allowed workspace-wide because they demand
  doc comments. Any other `#[allow]` is local and on the narrowest item.
- **Stubs never panic.** No `todo!()` or `unimplemented!()`. An unbuilt path returns a specific
  error variant such as `BackendNotCompiled { backend }`.
