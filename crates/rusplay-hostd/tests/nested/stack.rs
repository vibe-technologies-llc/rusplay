use std::{
    env, fs,
    io::{BufRead, BufReader},
    os::unix::{net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal, kill_process_group};
use tempfile::TempDir;

use super::{hardware, input::FakeInput};

static NEXT_STACK: AtomicU32 = AtomicU32::new(0);

const COMPOSITOR_STARTUP: Duration = Duration::from_secs(20);
const DIALOG_RETRY: Duration = Duration::from_millis(700);
const DIALOG_POINTER: (u32, u32) = (500, 300);
const KEY_ENTER: u32 = 28;
const PRIVATE_BUS_PREFIX: &str = "unix:";

pub struct Stack {
    pub dir: TempDir,
    pub runtime: PathBuf,
    pub socket_name: String,
    pub bus_address: String,
    session: Child,
    compositor: Child,
}

impl Stack {
    pub fn start() -> Option<Self> {
        let Some(runtime) = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
            hardware::missing("XDG_RUNTIME_DIR");
            return None;
        };
        if !runtime.join("pipewire-0").exists() {
            hardware::missing("a running PipeWire daemon");
            return None;
        }

        let dir = tempfile::Builder::new().prefix("rpn").tempdir().unwrap();
        let socket_name = format!(
            "rusplay-nested-{}-{}",
            std::process::id(),
            NEXT_STACK.fetch_add(1, Ordering::Relaxed)
        );
        for name in ["config", "data", "state", "cache"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }

        let spawned = Self::base(&dir, &socket_name, "dbus-run-session")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .args([
                "--",
                "sh",
                "-c",
                "printf '%s\\n' \"$DBUS_SESSION_BUS_ADDRESS\"; exec sleep 86400",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        let Ok(mut session) = spawned else {
            hardware::missing("dbus-run-session");
            return None;
        };

        let mut bus_address = String::new();
        BufReader::new(session.stdout.take().unwrap())
            .read_line(&mut bus_address)
            .unwrap();
        let bus_address = bus_address.trim().to_owned();
        assert!(
            bus_address.starts_with(PRIVATE_BUS_PREFIX)
                && env::var("DBUS_SESSION_BUS_ADDRESS").ok().as_deref() != Some(&bus_address),
            "the nested stack must run on its own private bus, got {bus_address:?}"
        );

        let spawned = Self::base(&dir, &socket_name, "kwin_wayland")
            .env("DBUS_SESSION_BUS_ADDRESS", &bus_address)
            .env_remove("WAYLAND_DISPLAY")
            .args(["--virtual", "--socket", &socket_name])
            .args([
                "--no-lockscreen",
                "--no-global-shortcuts",
                "--no-kactivities",
            ])
            .stdin(Stdio::null())
            .stdout(fs::File::create(dir.path().join("kwin.log")).unwrap())
            .stderr(fs::File::create(dir.path().join("kwin.err")).unwrap())
            .process_group(0)
            .spawn();
        let Ok(compositor) = spawned else {
            terminate(&mut session);
            hardware::missing("kwin_wayland");
            return None;
        };

        let mut stack = Self {
            dir,
            runtime,
            socket_name,
            bus_address,
            session,
            compositor,
        };

        let deadline = Instant::now() + COMPOSITOR_STARTUP;
        while !stack.socket().exists() {
            if Instant::now() >= deadline {
                hardware::missing("a nested kwin_wayland that opens its socket");
                stack.shutdown();
                return None;
            }
            thread::sleep(Duration::from_millis(50));
        }

        Some(stack)
    }

    fn base(dir: &TempDir, socket_name: &str, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("XDG_DATA_HOME", dir.path().join("data"))
            .env("XDG_STATE_HOME", dir.path().join("state"))
            .env("XDG_CACHE_HOME", dir.path().join("cache"))
            .env("XDG_CURRENT_DESKTOP", "KDE")
            .env("XDG_SESSION_TYPE", "wayland")
            .env("KDE_FULL_SESSION", "true")
            .env("WAYLAND_DISPLAY", socket_name)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_SOCKET");
        command
    }

    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let program = program.as_ref().to_string_lossy().into_owned();
        let mut command = Self::base(&self.dir, &self.socket_name, &program);
        command.env("DBUS_SESSION_BUS_ADDRESS", &self.bus_address);
        command
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime.join(&self.socket_name)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn connect_wayland(&self) -> UnixStream {
        UnixStream::connect(self.socket()).unwrap()
    }

    pub fn answer_dialog_until(&self, done: impl Fn() -> bool, within: Duration) -> bool {
        let input = FakeInput::connect(self);
        let deadline = Instant::now() + within;

        while Instant::now() < deadline {
            if done() {
                return true;
            }
            input.pointer_to(DIALOG_POINTER.0, DIALOG_POINTER.1);
            input.press(KEY_ENTER);
            thread::sleep(DIALOG_RETRY);
        }

        done()
    }

    pub fn shutdown(&mut self) {
        terminate(&mut self.compositor);
        terminate(&mut self.session);
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn terminate(child: &mut Child) {
    if let Some(group) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
        kill_process_group(group, Signal::TERM).ok();
        thread::sleep(Duration::from_millis(200));
        kill_process_group(group, Signal::KILL).ok();
    }
    child.wait().ok();
}

pub fn wait_for(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }

    condition()
}

pub fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
}
