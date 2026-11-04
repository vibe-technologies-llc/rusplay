use std::{
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader},
    net::SocketAddr,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::Arc,
    thread,
    time::Duration,
};

use parking_lot::Mutex;
use rustix::process::{Pid, Signal, kill_process};

use super::stack::{Stack, wait_for};

const STOP_GRACE: Duration = Duration::from_secs(5);

pub struct Daemon {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
    environment: Vec<(OsString, Option<OsString>)>,
    config: PathBuf,
}

pub struct DaemonOptions {
    pub codec: &'static str,
    pub backend: &'static str,
    pub extra: String,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            codec: "av1",
            backend: "svt-av1",
            extra: String::new(),
        }
    }
}

impl Daemon {
    pub fn config_path(stack: &Stack) -> PathBuf {
        stack.path("host.toml")
    }

    pub fn pin_path(stack: &Stack) -> PathBuf {
        stack.path("pin")
    }

    pub fn write_config(stack: &Stack, options: &DaemonOptions) -> PathBuf {
        let text = format!(
            r#"[video]
codec = "{codec}"
bitrate_kbps = 20000

[video.pixel_format]
chroma = "4:2:0"
depth = "eight"

[video.encoder]
backend = "{backend}"

[network]
bind_address = "127.0.0.1"
port = 0
discovery_port = 47811
stats_log_interval_ms = 1000

[peers]
write_pin_file = true
pin_file = "{pin}"
host_file = "{peers}"
control_socket_path = "{control}"

[input]
accept_remote = false
gamepads = false

[capture]
source = "virtual"
restore_token_file = "{token}"

{extra}
"#,
            codec = options.codec,
            backend = options.backend,
            pin = Self::pin_path(stack).display(),
            peers = stack.path("peers-host.toml").display(),
            control = stack.path("control.sock").display(),
            token = stack.path("restore-token").display(),
            extra = options.extra,
        );
        let path = Self::config_path(stack);
        std::fs::write(&path, text).unwrap();

        path
    }

    pub fn start(stack: &Stack, config: PathBuf) -> Self {
        let mut command = stack.command(env!("CARGO_BIN_EXE_rusplay-hostd"));
        let mut child = command
            .arg("--config")
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let lines = Arc::new(Mutex::new(Vec::new()));
        let reader = BufReader::new(child.stderr.take().unwrap());
        thread::spawn({
            let lines = Arc::clone(&lines);
            move || {
                for line in reader.lines().map_while(Result::ok) {
                    lines.lock().push(line);
                }
            }
        });

        let environment = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
            .collect();

        Self {
            child,
            lines,
            environment,
            config,
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().clone()
    }

    pub fn find(&self, needle: &str) -> Option<String> {
        self.lines
            .lock()
            .iter()
            .find(|line| line.contains(needle))
            .cloned()
    }

    pub fn count(&self, needle: &str) -> usize {
        self.lines
            .lock()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    pub fn wait_for_line(&self, needle: &str, within: Duration) -> Option<String> {
        wait_for(within, || self.find(needle).is_some());
        self.find(needle)
    }

    pub fn address(&self, within: Duration) -> Option<SocketAddr> {
        let line = self.wait_for_line("listening on ", within)?;
        line.rsplit("listening on ").next()?.trim().parse().ok()
    }

    pub fn control(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rusplay-hostd"));
        for (key, value) in &self.environment {
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        command
            .arg("--config")
            .arg(&self.config)
            .args(arguments)
            .output()
            .unwrap()
    }

    pub fn stop(&mut self) {
        if let Some(pid) = i32::try_from(self.child.id()).ok().and_then(Pid::from_raw) {
            kill_process(pid, Signal::TERM).ok();
        }
        if !wait_for(STOP_GRACE, || self.child.try_wait().unwrap().is_some()) {
            self.child.kill().ok();
        }
        self.child.wait().ok();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}
