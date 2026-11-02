use std::{
    env,
    fmt::Display,
    io::{self, Write},
    thread,
};

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
