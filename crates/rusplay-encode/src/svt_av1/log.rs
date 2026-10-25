use std::{
    ffi::{CStr, c_char, c_int, c_void},
    io::{self, Write},
    ptr,
    sync::atomic::{AtomicI32, Ordering},
};

use parking_lot::Once;
use rusplay_config::SvtAv1LogLevel;

use super::sys;

const MESSAGE_CAPACITY: usize = 1024;
const OFF: i32 = -1;

static THRESHOLD: AtomicI32 = AtomicI32::new(sys::SvtAv1LogLevel_SVT_AV1_LOG_WARN);
static REGISTRATION: Once = Once::new();

unsafe extern "C" {
    fn vsnprintf(
        buffer: *mut c_char,
        capacity: usize,
        format: *const c_char,
        arguments: *mut sys::__va_list_tag,
    ) -> c_int;
}

const fn threshold_of(level: SvtAv1LogLevel) -> i32 {
    match level {
        SvtAv1LogLevel::Off => OFF,
        SvtAv1LogLevel::Fatal => sys::SvtAv1LogLevel_SVT_AV1_LOG_FATAL,
        SvtAv1LogLevel::Error => sys::SvtAv1LogLevel_SVT_AV1_LOG_ERROR,
        SvtAv1LogLevel::Warn => sys::SvtAv1LogLevel_SVT_AV1_LOG_WARN,
        SvtAv1LogLevel::Info => sys::SvtAv1LogLevel_SVT_AV1_LOG_INFO,
        SvtAv1LogLevel::Debug => sys::SvtAv1LogLevel_SVT_AV1_LOG_DEBUG,
    }
}

const fn label_of(level: sys::SvtAv1LogLevel) -> &'static str {
    match level {
        sys::SvtAv1LogLevel_SVT_AV1_LOG_FATAL => "fatal",
        sys::SvtAv1LogLevel_SVT_AV1_LOG_ERROR => "error",
        sys::SvtAv1LogLevel_SVT_AV1_LOG_WARN => "warn",
        sys::SvtAv1LogLevel_SVT_AV1_LOG_INFO => "info",
        _ => "debug",
    }
}

pub fn install(level: SvtAv1LogLevel) {
    THRESHOLD.store(threshold_of(level), Ordering::Relaxed);

    REGISTRATION.call_once(|| {
        unsafe { sys::svt_av1_set_log_callback(Some(forward), ptr::null_mut()) };
    });
}

unsafe extern "C" fn forward(
    _context: *mut c_void,
    level: sys::SvtAv1LogLevel,
    tag: *const c_char,
    format: *const c_char,
    arguments: *mut sys::__va_list_tag,
) {
    if level > THRESHOLD.load(Ordering::Relaxed) || format.is_null() {
        return;
    }

    let mut buffer: [c_char; MESSAGE_CAPACITY] = [0; MESSAGE_CAPACITY];
    let written = unsafe { vsnprintf(buffer.as_mut_ptr(), buffer.len(), format, arguments) };
    if written < 0 {
        return;
    }

    let message = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
    let tag = if tag.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(tag) }
            .to_string_lossy()
            .into_owned()
    };

    let line = format!(
        "svt-av1 {}{}{}: {}\n",
        label_of(level),
        if tag.is_empty() { "" } else { " " },
        tag,
        message.trim_end()
    );
    let _ = io::stderr().write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_follow_the_level_order() {
        let levels = [
            SvtAv1LogLevel::Off,
            SvtAv1LogLevel::Fatal,
            SvtAv1LogLevel::Error,
            SvtAv1LogLevel::Warn,
            SvtAv1LogLevel::Info,
            SvtAv1LogLevel::Debug,
        ];

        let thresholds: Vec<i32> = levels.iter().map(|level| threshold_of(*level)).collect();

        assert!(thresholds.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(thresholds[0], OFF);
    }

    #[test]
    fn labels_name_every_library_level() {
        assert_eq!(label_of(sys::SvtAv1LogLevel_SVT_AV1_LOG_WARN), "warn");
        assert_eq!(label_of(sys::SvtAv1LogLevel_SVT_AV1_LOG_FATAL), "fatal");
    }
}
