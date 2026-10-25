---
name: check
description: Run Rusplay's full verification (format, comment scan, clippy, tests). Use before every commit and after any change to Rust, WGSL or manifests.
---

# Check

Run from the repository root and stop at the first failure:

1. `rust-formatter` to format Rust and TOML, then `rust-formatter --check` to confirm it is clean.
   Never `cargo fmt`.
2. `scripts/check-comments.sh`. Any output is a comment that must go.
3. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
4. `cargo test --workspace --all-features`

Fix failures at the source. Do not add `#[allow]` to quiet clippy unless the lint is wrong for that
exact item, and then on the narrowest item.
