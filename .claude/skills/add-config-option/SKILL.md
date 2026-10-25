---
name: add-config-option
description: Add a user-facing tunable to rusplay-config with its default, validation and docs. Use whenever behaviour should become configurable.
---

# Add a config option

1. Put the field in the struct for its section in `crates/rusplay-config/src/<section>.rs`.
   A closed choice is an enum with `#[serde(rename_all = "kebab-case")]`, not a string.
2. Set its default in that struct's `Default` impl. The default is the value that ships.
3. If serde cannot express its valid range, add a `ConfigError` variant carrying the bad value
   and check it in the section's `validate`.
4. Re-export any new public type from `crates/rusplay-config/src/lib.rs`.
5. Add a row for it to `FIELDS` in `crates/rusplay-app/src/settings/schema.rs`, and a
   `ConfigError::field` arm for any new error variant. The app's tests fail until both exist.
6. Add a test when parsing or validation is non-trivial. `empty_document_yields_defaults` must
   keep passing.
7. Wire the option to the code that reads it, and mention it in the subsystem's rules file if
   it changes the design.
8. Run the `check` skill.
