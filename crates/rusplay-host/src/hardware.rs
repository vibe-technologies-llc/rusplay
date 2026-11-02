use std::{
    env,
    fmt::Display,
    io::{self, Write},
    thread,
};

use rusplay_config::{Config, VideoEncoderConfig};
use rusplay_encode::{EncodeError, capabilities};

const REQUIRE_VARIABLE: &str = "RUSPLAY_REQUIRE_HW";

fn required() -> bool {
    env::var(REQUIRE_VARIABLE).is_ok_and(|value| value == "1")
}

pub(crate) fn require<T>(found: Option<T>, what: impl Display) -> Option<T> {
    if found.is_none() {
        missing(what);
    }

    found
}

pub(crate) fn missing(what: impl Display) {
    assert!(
        !required(),
        "{}: {what} is missing and {REQUIRE_VARIABLE}=1",
        test_name(),
    );

    unsupported(what);
}

pub(crate) fn unsupported(what: impl Display) {
    let _ = writeln!(io::stderr(), "skipped {}: {what}", test_name());
}

fn test_name() -> String {
    thread::current().name().unwrap_or("<unnamed>").to_owned()
}

pub(crate) fn encoder_supports_configured_format(config: &Config) -> bool {
    match capabilities(&config.video.encoder) {
        Ok(found) if found.supports(config.video.codec, config.video.pixel_format) => true,
        Ok(_) => {
            unsupported("the configured codec and pixel format");
            false
        }
        Err(error)
            if is_hardware(&config.video.encoder)
                && !matches!(error, EncodeError::BackendNotCompiled { .. }) =>
        {
            missing(error);
            false
        }
        Err(error) => {
            unsupported(error);
            false
        }
    }
}

fn is_hardware(encoder: &VideoEncoderConfig) -> bool {
    matches!(
        encoder,
        VideoEncoderConfig::Vaapi(_) | VideoEncoderConfig::Nvenc(_)
    )
}
