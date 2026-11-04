#![cfg(feature = "wayland")]

#[path = "../src/hardware.rs"]
mod hardware;

use std::{
    env,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, Instant},
};

use rusplay_clipboard::{BackendKind, ClipboardBackend, DataControl, DataControlOptions};
use rustix::process::{Pid, Signal, kill_process_group};

static NEXT_SOCKET: AtomicU32 = AtomicU32::new(0);

const STARTUP: Duration = Duration::from_secs(15);
const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(600);

struct NestedCompositor {
    child: Child,
    socket: PathBuf,
}

impl NestedCompositor {
    fn start() -> Option<Self> {
        let Some(runtime) = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
            hardware::missing("XDG_RUNTIME_DIR");
            return None;
        };
        let name = format!(
            "rusplay-clipboard-test-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        );
        let socket = runtime.join(&name);

        let spawned = Command::new("dbus-run-session")
            .args(["--", "kwin_wayland", "--virtual", "--socket", &name])
            .args([
                "--no-lockscreen",
                "--no-global-shortcuts",
                "--no-kactivities",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        let Ok(child) = spawned else {
            hardware::missing("dbus-run-session and kwin_wayland for a nested compositor");
            return None;
        };

        let compositor = Self { child, socket };
        let deadline = Instant::now() + STARTUP;
        while !compositor.socket.exists() {
            if Instant::now() >= deadline {
                hardware::missing("a nested kwin_wayland that opens its socket");
                return None;
            }
            thread::sleep(Duration::from_millis(50));
        }

        Some(compositor)
    }

    fn open(&self, max_bytes: usize, watch: bool) -> DataControl {
        DataControl::open_at(&self.socket, DataControlOptions { max_bytes, watch }).unwrap()
    }
}

impl Drop for NestedCompositor {
    fn drop(&mut self) {
        if let Some(group) = i32::try_from(self.child.id()).ok().and_then(Pid::from_raw) {
            kill_process_group(group, Signal::TERM).ok();
        }
        self.child.wait().ok();
        std::fs::remove_file(&self.socket).ok();
        std::fs::remove_file(self.socket.with_extension("lock")).ok();
    }
}

fn next_change(backend: &mut DataControl, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;

    while Instant::now() < deadline {
        if let Some(text) = backend.poll_change().unwrap() {
            return Some(text);
        }
        thread::sleep(Duration::from_millis(10));
    }

    None
}

#[test]
fn a_selection_set_on_one_connection_reaches_a_watcher_on_another() {
    let Some(compositor) = NestedCompositor::start() else {
        return;
    };
    let mut setter = compositor.open(1024, false);
    let mut watcher = compositor.open(1024, true);

    setter.set_text("héllo from the setter").unwrap();

    assert_eq!(setter.kind(), BackendKind::ExtDataControl);
    assert_eq!(
        next_change(&mut watcher, WAIT).as_deref(),
        Some("héllo from the setter")
    );
}

#[test]
fn text_over_the_limit_is_dropped_and_the_next_selection_still_arrives() {
    let Some(compositor) = NestedCompositor::start() else {
        return;
    };
    let mut setter = compositor.open(1024, false);
    let mut watcher = compositor.open(16, true);

    setter.set_text(&"x".repeat(100)).unwrap();
    assert_eq!(next_change(&mut watcher, QUIET), None);

    setter.set_text("short").unwrap();
    assert_eq!(next_change(&mut watcher, WAIT).as_deref(), Some("short"));
}

#[test]
fn a_connection_that_does_not_watch_never_reads_the_selection() {
    let Some(compositor) = NestedCompositor::start() else {
        return;
    };
    let mut setter = compositor.open(1024, false);
    let mut deaf = compositor.open(1024, false);

    setter.set_text("not for you").unwrap();

    assert_eq!(next_change(&mut deaf, QUIET), None);
}

#[test]
fn the_selection_in_place_before_the_connection_opened_is_not_reported() {
    let Some(compositor) = NestedCompositor::start() else {
        return;
    };
    let mut setter = compositor.open(1024, false);
    setter.set_text("already there").unwrap();
    thread::sleep(QUIET);

    let mut late = compositor.open(1024, true);

    assert_eq!(next_change(&mut late, QUIET), None);
}

#[test]
fn replacing_a_selection_cancels_the_old_source_and_serves_the_new_text() {
    let Some(compositor) = NestedCompositor::start() else {
        return;
    };
    let mut first = compositor.open(1024, false);
    let mut second = compositor.open(1024, false);
    let mut watcher = compositor.open(1024, true);

    first.set_text("one").unwrap();
    assert_eq!(next_change(&mut watcher, WAIT).as_deref(), Some("one"));
    second.set_text("two").unwrap();

    assert_eq!(next_change(&mut watcher, WAIT).as_deref(), Some("two"));
}
