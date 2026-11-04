mod flavour;

use std::{
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
};
use wayland_client::{
    ConnectError, Connection, EventQueue, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry::WlRegistry,
};

use self::flavour::flavour;
use crate::{
    BackendKind, ClipboardBackend, ClipboardError,
    transfer::{ReadJob, ReadStep},
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const THREAD_NAME: &str = "rusplay-clipboard";
const EXT_GLOBAL: &str = "ext_data_control_manager_v1";
const WLR_GLOBAL: &str = "zwlr_data_control_manager_v1";

flavour!(
    ext,
    protocol: wayland_protocols::ext::data_control::v1::client,
    manager: ext_data_control_manager_v1::ExtDataControlManagerV1,
    device: ext_data_control_device_v1::ExtDataControlDeviceV1,
    source: ext_data_control_source_v1::ExtDataControlSourceV1,
    offer: ext_data_control_offer_v1::ExtDataControlOfferV1,
    global: "ext_data_control_manager_v1",
    kind: BackendKind::ExtDataControl,
);

flavour!(
    wlr,
    protocol: wayland_protocols_wlr::data_control::v1::client,
    manager: zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    device: zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
    source: zwlr_data_control_source_v1::ZwlrDataControlSourceV1,
    offer: zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    global: "zwlr_data_control_manager_v1",
    kind: BackendKind::WlrDataControl,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataControlOptions {
    pub max_bytes: usize,
    pub watch: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Start {
    pub max_bytes: usize,
    pub watch: bool,
    pub changes: Sender<String>,
}

#[derive(Debug)]
pub(crate) struct Session {
    max_bytes: usize,
    watch: bool,
    armed: bool,
    jobs: Vec<ReadJob>,
    changes: Sender<String>,
}

impl Session {
    pub(crate) fn new(options: Start) -> Self {
        Self {
            max_bytes: options.max_bytes,
            watch: options.watch,
            armed: false,
            jobs: Vec::new(),
            changes: options.changes,
        }
    }

    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }

    pub(crate) fn is_armed(&self) -> bool {
        self.armed && self.watch
    }

    pub(crate) fn begin_read(&mut self, now: Instant) -> std::io::Result<OwnedFd> {
        let (job, writer) = ReadJob::open(self.max_bytes, now)?;
        self.jobs.push(job);

        Ok(writer)
    }

    fn job_fds(&self) -> Vec<BorrowedFd<'_>> {
        self.jobs.iter().map(|job| job.fd().as_fd()).collect()
    }

    fn pump(&mut self, now: Instant) {
        for mut job in std::mem::take(&mut self.jobs) {
            match job.advance(now) {
                ReadStep::Pending => self.jobs.push(job),
                ReadStep::Done => {
                    if let Some(text) = job.into_text() {
                        self.changes.send(text).ok();
                    }
                }
                ReadStep::Abandoned => {}
            }
        }
    }
}

pub(crate) trait Flavour {
    fn session(&mut self) -> &mut Session;

    fn is_finished(&self) -> bool;

    fn set_text(&mut self, text: &str, handle: &QueueHandle<Self>)
    where
        Self: Sized;

    fn shut_down(&mut self);
}

#[derive(Debug)]
pub(crate) struct Started {
    kind: BackendKind,
    commands: Sender<String>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Result<(), ClipboardError>>,
}

impl Started {
    pub(crate) fn spawn<S: Flavour + Send + 'static>(
        connection: Connection,
        queue: EventQueue<S>,
        state: S,
        kind: BackendKind,
    ) -> Result<Self, ClipboardError> {
        let (commands, queued) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || serve(&connection, queue, state, &queued, &worker_stop))
            .map_err(ClipboardError::ThreadSpawn)?;

        Ok(Self {
            kind,
            commands,
            stop,
            thread,
        })
    }
}

fn serve<S: Flavour>(
    connection: &Connection,
    mut queue: EventQueue<S>,
    mut state: S,
    commands: &Receiver<String>,
    stop: &AtomicBool,
) -> Result<(), ClipboardError> {
    let handle = queue.handle();

    while !stop.load(Ordering::Relaxed) {
        while let Ok(text) = commands.try_recv() {
            state.set_text(&text, &handle);
        }

        queue
            .dispatch_pending(&mut state)
            .map_err(|error| ClipboardError::WaylandDispatch(Box::new(error)))?;
        connection
            .flush()
            .map_err(|error| ClipboardError::WaylandRead(Box::new(error)))?;
        state.session().pump(Instant::now());
        if state.is_finished() {
            return Err(ClipboardError::BackendStopped);
        }

        let Some(guard) = connection.prepare_read() else {
            continue;
        };

        let limit = Timespec {
            tv_sec: 0,
            tv_nsec: i64::from(POLL_INTERVAL.subsec_nanos()),
        };
        let ready = {
            let socket = guard.connection_fd();
            let job_fds = state.session().job_fds();
            let mut fds = vec![PollFd::new(&socket, PollFlags::IN)];
            fds.extend(job_fds.iter().map(|fd| PollFd::new(fd, PollFlags::IN)));

            match poll(&mut fds, Some(&limit)) {
                Ok(_) => fds[0].revents().contains(PollFlags::IN),
                Err(Errno::INTR) => false,
                Err(errno) => return Err(ClipboardError::WaylandPoll(errno.into())),
            }
        };

        if ready {
            guard
                .read()
                .map_err(|error| ClipboardError::WaylandRead(Box::new(error)))?;
        }
    }

    state.shut_down();
    connection
        .flush()
        .map_err(|error| ClipboardError::WaylandRead(Box::new(error)))
}

#[derive(Debug)]
pub struct DataControl {
    started: Option<Started>,
    changes: Receiver<String>,
}

impl DataControl {
    pub fn open(options: DataControlOptions) -> Result<Self, ClipboardError> {
        Self::open_on(&connect()?, options)
    }

    pub fn open_at(socket: &Path, options: DataControlOptions) -> Result<Self, ClipboardError> {
        let stream = UnixStream::connect(socket).map_err(ClipboardError::WaylandSocket)?;
        let connection = Connection::from_socket(stream)
            .map_err(|error| ClipboardError::WaylandConnect(Box::new(error)))?;

        Self::open_on(&connection, options)
    }

    fn open_on(
        connection: &Connection,
        options: DataControlOptions,
    ) -> Result<Self, ClipboardError> {
        let (changes_sender, changes) = mpsc::channel();
        let start = Start {
            max_bytes: options.max_bytes,
            watch: options.watch,
            changes: changes_sender,
        };

        let started = match ext::start(connection, start.clone())? {
            Some(started) => started,
            None => wlr::start(connection, start)?.ok_or(ClipboardError::DataControlMissing)?,
        };

        Ok(Self {
            started: Some(started),
            changes,
        })
    }

    fn finished(&mut self) -> Option<ClipboardError> {
        if !self.started.as_ref()?.thread.is_finished() {
            return None;
        }

        let started = self.started.take()?;
        Some(
            started
                .thread
                .join()
                .ok()
                .and_then(Result::err)
                .unwrap_or(ClipboardError::BackendStopped),
        )
    }
}

impl ClipboardBackend for DataControl {
    fn kind(&self) -> BackendKind {
        self.started
            .as_ref()
            .map_or(BackendKind::ExtDataControl, |started| started.kind)
    }

    fn poll_change(&mut self) -> Result<Option<String>, ClipboardError> {
        if let Some(error) = self.finished() {
            return Err(error);
        }

        Ok(self.changes.try_recv().ok())
    }

    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        if let Some(error) = self.finished() {
            return Err(error);
        }

        let started = self
            .started
            .as_ref()
            .ok_or(ClipboardError::BackendStopped)?;
        started
            .commands
            .send(text.to_owned())
            .map_err(|_| ClipboardError::BackendStopped)
    }
}

impl Drop for DataControl {
    fn drop(&mut self) {
        if let Some(started) = self.started.take() {
            started.stop.store(true, Ordering::Relaxed);
            started.thread.join().ok();
        }
    }
}

struct Probe;

impl wayland_client::Dispatch<WlRegistry, GlobalListContents> for Probe {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

pub fn probe() -> Result<BackendKind, ClipboardError> {
    let connection = connect()?;
    let (globals, _queue) = registry_queue_init::<Probe>(&connection)
        .map_err(|error| ClipboardError::WaylandGlobals(Box::new(error)))?;
    let offers = |interface: &str| {
        globals
            .contents()
            .with_list(|list| list.iter().any(|global| global.interface == interface))
    };

    if offers(EXT_GLOBAL) {
        Ok(BackendKind::ExtDataControl)
    } else if offers(WLR_GLOBAL) {
        Ok(BackendKind::WlrDataControl)
    } else {
        Err(ClipboardError::DataControlMissing)
    }
}

fn connect() -> Result<Connection, ClipboardError> {
    Connection::connect_to_env().map_err(|error| match error {
        ConnectError::NoCompositor => ClipboardError::NoWaylandDisplay,
        other => ClipboardError::WaylandConnect(Box::new(other)),
    })
}
