use std::{
    env,
    fmt::Display,
    fs::OpenOptions,
    io::{self, Write},
    thread,
};

const REQUIRE_VARIABLE: &str = "RUSPLAY_REQUIRE_HW";
const SKIP_LOG_VARIABLE: &str = "RUSPLAY_SKIP_LOG";

fn required() -> bool {
    env::var(REQUIRE_VARIABLE).is_ok_and(|value| value == "1")
}

pub(crate) fn require<T>(found: Option<T>, what: impl Display) -> Option<T> {
    if found.is_none() {
        missing(what);
    }

    found
}

#[cfg(all(feature = "software", feature = "vaapi"))]
pub(crate) fn device_present(path: &std::path::Path) -> bool {
    let present = path.exists();

    if !present {
        missing(format_args!("the render node {}", path.display()));
    }

    present
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
    report_skip(&format!("skipped {}: {what}\n", test_name()));
}

fn test_name() -> String {
    thread::current().name().unwrap_or("<unnamed>").to_owned()
}

fn report_skip(line: &str) {
    let _ = io::stderr().write_all(line.as_bytes());

    if let Some(path) = env::var_os(SKIP_LOG_VARIABLE)
        && let Ok(mut log) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = log.write_all(line.as_bytes());
    }
}
