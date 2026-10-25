# Errors

- Each crate has one public error enum (`ConfigError`, `CaptureError`, `EncodeError`, ...),
  derived with `thiserror`.
- One variant per failure, named for what went wrong and carrying typed data:
  `MtuOutOfRange(u16)`, `Bind { addr, source }`, `CodecUnsupported { backend, codec }`. A new
  failure mode gets a new variant.
- Never a catch-all such as `Other(String)`, `Generic(String)` or `Custom(String)`. A
  `&'static str` reason is allowed only where an upstream crate reports failures as bare strings
  (opus-rs), and then only inside a variant naming the operation (`OpusEncode(&'static str)`).
- Wrapped sources use `#[source]`/`#[from]` so `rusplay-hostd` can print the cause chain.
  Composing crates (`rusplay-host`, `rusplay-client`) wrap lower errors with
  `#[error(transparent)]` and `#[from]`.
- Display strings are lowercase, have no trailing period, and do not repeat the source's
  message.
