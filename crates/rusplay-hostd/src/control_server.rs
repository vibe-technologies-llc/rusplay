use std::{
    fmt::Write as _,
    fs,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr},
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use rusplay_host::{HostError, PeerStore, Pin};

use crate::{
    control::{self, ControlError, Refusal, Request},
    peers::{self, PeersError},
};

const DIRECTORY_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;
const ACCEPT_POLL: Duration = Duration::from_millis(50);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(1);
const DRAIN_TIMEOUT: Duration = Duration::from_millis(200);
const DRAIN_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Pairing { opened: Instant, timeout: Duration },
    Streaming { client: SocketAddr },
}

#[derive(Debug, Clone)]
pub struct SharedPhase(Arc<Mutex<Phase>>);

impl SharedPhase {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Phase::Starting)))
    }

    pub fn get(&self) -> Phase {
        *self.0.lock()
    }

    pub fn set(&self, phase: Phase) {
        *self.0.lock() = phase;
    }
}

impl Default for SharedPhase {
    fn default() -> Self {
        Self::new()
    }
}

pub type PinRequest = Arc<dyn Fn(Pin) -> Result<(), HostError> + Send + Sync>;

#[derive(Clone)]
pub struct ControlContext {
    pub peers: Arc<RwLock<PeerStore>>,
    pub address: SocketAddr,
    pub pin_digits: u8,
    pub phase: SharedPhase,
    pub request_pin: PinRequest,
}

impl ControlContext {
    pub fn handle(&self, request: &Request) -> Result<String, Refusal> {
        match request {
            Request::Status => Ok(self.status()),
            Request::NewPin => self.new_pin(),
            Request::Forget { target } => self.forget(target),
        }
    }

    fn status(&self) -> String {
        let mut body = String::new();
        writeln!(body, "listening {}", self.address).ok();

        match self.phase.get() {
            Phase::Starting => writeln!(body, "state starting").ok(),
            Phase::Pairing { opened, timeout } => {
                let remaining = timeout.saturating_sub(opened.elapsed());
                writeln!(body, "state pairing").ok();
                writeln!(body, "window_remaining_s {}", remaining.as_secs()).ok()
            }
            Phase::Streaming { client } => writeln!(body, "state streaming {client}").ok(),
        };

        writeln!(body, "paired_clients {}", self.peers.read().len()).ok();

        body
    }

    fn new_pin(&self) -> Result<String, Refusal> {
        let pin = Pin::generate(self.pin_digits).map_err(|_| Refusal::PinUnavailable)?;
        (self.request_pin)(pin.clone()).map_err(|error| match error {
            HostError::StreamActive => Refusal::Streaming,
            _ => Refusal::PinUnavailable,
        })?;

        Ok(format!("{}\n", pin.as_str()))
    }

    fn forget(&self, target: &str) -> Result<String, Refusal> {
        let mut store = self.peers.write();
        if store.is_stale() {
            store.refresh().map_err(|error| store_failed(&error))?;
        }

        let id = peers::resolve(&store, target).map_err(|error| match error {
            PeersError::NotFound { target } => Refusal::NotFound { target },
            PeersError::Ambiguous { target, count } => Refusal::Ambiguous { target, count },
        })?;
        let name = store
            .get(&id)
            .map(|record| record.name().to_owned())
            .unwrap_or_default();
        store.forget(&id).map_err(|error| store_failed(&error))?;

        Ok(format!("forgot {id} {name}\n"))
    }
}

fn store_failed(error: &dyn std::error::Error) -> Refusal {
    eprintln!("rusplay-hostd: control: {error}");
    Refusal::StoreFailed
}

#[derive(Debug)]
pub struct ControlServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlServer {
    pub fn start(path: &Path, context: ControlContext) -> Result<Self, ControlError> {
        if let Some(parent) = path.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(DIRECTORY_MODE)
                .create(parent)
                .map_err(|source| ControlError::CreateDirectory {
                    path: parent.to_owned(),
                    source,
                })?;
        }
        control::remove_stale(path)?;

        let bind_error = |source| ControlError::Bind {
            path: path.to_owned(),
            source,
        };
        let listener = UnixListener::bind(path).map_err(bind_error)?;
        fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))
            .and_then(|()| listener.set_nonblocking(true))
            .map_err(bind_error)?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("rusplay-hostd-control".to_owned())
                .spawn(move || accept_loop(&listener, &context, &stop))
                .map_err(ControlError::Spawn)?
        };

        Ok(Self {
            path: path.to_owned(),
            stop,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
        fs::remove_file(&self.path).ok();
    }
}

fn accept_loop(listener: &UnixListener, context: &ControlContext, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = answer(&stream, context) {
                    eprintln!("rusplay-hostd: control: {error}");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(error) => {
                eprintln!("rusplay-hostd: control: {error}");
                thread::sleep(RETRY_DELAY);
            }
        }
    }
}

fn answer(stream: &UnixStream, context: &ControlContext) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(CONNECTION_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECTION_TIMEOUT))?;

    let reply = match control::read_request(stream)? {
        Ok(request) => context.handle(&request),
        Err(refusal) => Err(refusal),
    };

    let mut writer = stream;
    writer.write_all(control::render_reply(&reply).as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    drain(stream);

    Ok(())
}

fn drain(stream: &UnixStream) {
    if stream.set_read_timeout(Some(DRAIN_TIMEOUT)).is_err() {
        return;
    }

    let deadline = Instant::now() + DRAIN_TIMEOUT;
    let mut remaining = DRAIN_BYTES;
    let mut buffer = [0; 1024];
    let mut reader = stream;

    while remaining > 0 && Instant::now() < deadline {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => remaining = remaining.saturating_sub(read),
        }
    }
}

#[cfg(test)]
mod tests {
    use rusplay_host::PeerRecord;
    use rusplay_net::PeerSecret;
    use tempfile::TempDir;

    use super::*;
    use crate::control::{ControlError, send};

    struct Fixture {
        _directory: TempDir,
        socket: PathBuf,
        phase: SharedPhase,
        requested: Arc<Mutex<Vec<Pin>>>,
        streaming: Arc<AtomicBool>,
        peers: Arc<RwLock<PeerStore>>,
        server: ControlServer,
    }

    impl Fixture {
        fn new(names: &[(&str, u8)]) -> Self {
            let directory = TempDir::new().unwrap();
            let mut store = PeerStore::open(directory.path().join("paired-clients")).unwrap();
            for (name, byte) in names {
                let record = PeerRecord::new(name, PeerSecret::from_bytes([*byte; 32])).unwrap();
                store.remember(record).unwrap();
            }
            let peers = Arc::new(RwLock::new(store));
            let phase = SharedPhase::new();
            let requested = Arc::new(Mutex::new(Vec::new()));
            let streaming = Arc::new(AtomicBool::new(false));
            let socket = directory.path().join("run").join("control.sock");
            let server = ControlServer::start(
                &socket,
                ControlContext {
                    peers: Arc::clone(&peers),
                    address: "127.0.0.1:47990".parse().unwrap(),
                    pin_digits: 6,
                    phase: phase.clone(),
                    request_pin: recording(&requested, &streaming),
                },
            )
            .unwrap();

            Self {
                _directory: directory,
                socket,
                phase,
                requested,
                streaming,
                peers,
                server,
            }
        }
    }

    fn recording(requested: &Arc<Mutex<Vec<Pin>>>, streaming: &Arc<AtomicBool>) -> PinRequest {
        let requested = Arc::clone(requested);
        let streaming = Arc::clone(streaming);

        Arc::new(move |pin| {
            if streaming.load(Ordering::Acquire) {
                return Err(HostError::StreamActive);
            }
            requested.lock().push(pin);

            Ok(())
        })
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_socket_and_its_directory_are_private() {
        let fixture = Fixture::new(&[]);

        assert_eq!(mode(&fixture.socket), SOCKET_MODE);
        assert_eq!(mode(fixture.socket.parent().unwrap()), DIRECTORY_MODE);
    }

    #[test]
    fn status_reports_the_address_phase_and_paired_count() {
        let fixture = Fixture::new(&[("laptop", 1), ("desk pc", 2)]);

        let starting = send(&fixture.socket, &Request::Status).unwrap();
        fixture.phase.set(Phase::Pairing {
            opened: Instant::now(),
            timeout: Duration::from_secs(60),
        });
        let pairing = send(&fixture.socket, &Request::Status).unwrap();
        fixture.phase.set(Phase::Streaming {
            client: "10.0.0.2:5000".parse().unwrap(),
        });
        let streaming = send(&fixture.socket, &Request::Status).unwrap();

        assert_eq!(
            starting,
            "listening 127.0.0.1:47990\nstate starting\npaired_clients 2\n"
        );
        assert!(pairing.contains("state pairing\nwindow_remaining_s "));
        assert!(streaming.contains("state streaming 10.0.0.2:5000\n"));
    }

    #[test]
    fn a_new_pin_is_generated_handed_to_the_host_and_returned() {
        let fixture = Fixture::new(&[]);

        let reply = send(&fixture.socket, &Request::NewPin).unwrap();
        let second = send(&fixture.socket, &Request::NewPin).unwrap();

        let requested = fixture.requested.lock();
        assert_eq!(requested.len(), 2);
        assert_eq!(reply.trim(), requested[0].as_str());
        assert_eq!(second.trim(), requested[1].as_str());
        assert_eq!(reply.trim().len(), 6);
        assert_ne!(requested[0], requested[1]);
    }

    #[test]
    fn a_new_pin_is_refused_while_a_client_streams() {
        let fixture = Fixture::new(&[]);
        fixture.streaming.store(true, Ordering::Release);

        let result = send(&fixture.socket, &Request::NewPin);

        assert!(matches!(
            result,
            Err(ControlError::Refused(Refusal::Streaming))
        ));
        assert!(fixture.requested.lock().is_empty());
    }

    #[test]
    fn forget_removes_a_client_by_name_or_id_and_refuses_the_rest() {
        let fixture = Fixture::new(&[("laptop", 1), ("same", 2), ("same", 3)]);
        let id = PeerSecret::from_bytes([1; 32]).id().to_string();

        let by_name = send(
            &fixture.socket,
            &Request::Forget {
                target: "laptop".to_owned(),
            },
        )
        .unwrap();
        let unknown = send(
            &fixture.socket,
            &Request::Forget {
                target: "laptop".to_owned(),
            },
        );
        let ambiguous = send(
            &fixture.socket,
            &Request::Forget {
                target: "same".to_owned(),
            },
        );

        assert_eq!(by_name, format!("forgot {id} laptop\n"));
        assert!(matches!(
            unknown,
            Err(ControlError::Refused(Refusal::NotFound { .. }))
        ));
        assert!(matches!(
            ambiguous,
            Err(ControlError::Refused(Refusal::Ambiguous { count: 2, .. }))
        ));
        assert_eq!(fixture.peers.read().len(), 2);
    }

    #[test]
    fn an_oversized_or_unknown_request_is_refused_and_the_server_keeps_answering() {
        let fixture = Fixture::new(&[]);

        let mut stream = UnixStream::connect(&fixture.socket).unwrap();
        stream
            .write_all(&vec![b'a'; control::MAX_REQUEST_BYTES * 4])
            .unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        let unknown = send(
            &fixture.socket,
            &Request::Forget {
                target: String::new(),
            },
        );

        assert_eq!(reply, "err too-long\n");
        assert!(matches!(
            unknown,
            Err(ControlError::Refused(Refusal::ForgetTargetMissing))
        ));
        assert!(send(&fixture.socket, &Request::Status).is_ok());
    }

    #[test]
    fn dropping_the_server_removes_the_socket_and_a_stale_one_is_replaced() {
        let fixture = Fixture::new(&[]);
        let socket = fixture.socket.clone();
        let context = ControlContext {
            peers: Arc::clone(&fixture.peers),
            address: "127.0.0.1:1".parse().unwrap(),
            pin_digits: 6,
            phase: fixture.phase.clone(),
            request_pin: recording(&fixture.requested, &fixture.streaming),
        };

        let second = ControlServer::start(&socket, context.clone());
        assert!(matches!(second, Err(ControlError::AlreadyRunning { .. })));
        assert_eq!(fixture.server.path(), socket);
        let Fixture { server, .. } = fixture;
        drop(server);

        assert!(!socket.exists());
        assert!(matches!(
            send(&socket, &Request::Status),
            Err(ControlError::NotRunning { .. })
        ));
        drop(UnixListener::bind(&socket).unwrap());
        let replacement = ControlServer::start(&socket, context).unwrap();
        assert!(send(&socket, &Request::Status).is_ok());
        drop(replacement);
    }
}
