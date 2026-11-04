use std::{
    os::fd::OwnedFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use ashpd::desktop::clipboard::{Clipboard, RequestClipboardOptions, SetSelectionOptions};
use futures_util::{StreamExt, stream};
use parking_lot::Mutex;
use rusplay_input::libei::RemoteDesktopSession;
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use crate::{
    BackendKind, ClipboardBackend, ClipboardError,
    transfer::{PASSWORD_HINT_MIME, ReadJob, ReadStep, TEXT_MIME, TRANSFER_TIMEOUT, write_all},
};

const THREAD_NAME: &str = "rusplay-portal-clipboard";
const READ_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalStep {
    Connect,
    RequestClipboard,
    SetSelection,
    SelectionRead,
    SelectionWrite,
    Subscribe,
}

fn portal(step: PortalStep) -> impl FnOnce(ashpd::Error) -> ClipboardError {
    move |source| ClipboardError::Portal {
        step,
        source: Box::new(source),
    }
}

struct Subscriber {
    changes: Sender<String>,
    watch: bool,
    max_bytes: usize,
    token: u64,
}

struct Shared {
    subscriber: Mutex<Option<Subscriber>>,
    current: Mutex<Option<Arc<str>>>,
    alive: AtomicBool,
    next_token: Mutex<u64>,
}

enum Signal {
    Owner { is_ours: bool, mimes: Vec<String> },
    Transfer { mime: String, serial: u32 },
}

#[derive(Clone)]
pub struct PortalClipboard {
    shared: Arc<Shared>,
    commands: Sender<String>,
}

impl std::fmt::Debug for PortalClipboard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PortalClipboard")
            .finish_non_exhaustive()
    }
}

pub async fn request_clipboard(
    session: &RemoteDesktopSession,
) -> Result<Clipboard, ClipboardError> {
    let clipboard = Clipboard::new()
        .await
        .map_err(portal(PortalStep::Connect))?;
    clipboard
        .request(session.session(), RequestClipboardOptions::default())
        .await
        .map_err(portal(PortalStep::RequestClipboard))?;

    Ok(clipboard)
}

impl PortalClipboard {
    pub fn spawn(
        session: Arc<RemoteDesktopSession>,
        clipboard: Clipboard,
    ) -> Result<Self, ClipboardError> {
        let shared = Arc::new(Shared {
            subscriber: Mutex::new(None),
            current: Mutex::new(None),
            alive: AtomicBool::new(true),
            next_token: Mutex::new(0),
        });
        let clipboard = Arc::new(clipboard);
        let (commands, queued) = mpsc::channel();

        let signals = {
            let (shared, session, clipboard) = (
                Arc::clone(&shared),
                Arc::clone(&session),
                Arc::clone(&clipboard),
            );
            thread::Builder::new()
                .name(THREAD_NAME.to_owned())
                .spawn(move || {
                    pollster::block_on(listen(&shared, &session, &clipboard)).ok();
                    shared.alive.store(false, Ordering::Relaxed);
                })
                .map_err(ClipboardError::PortalSpawn)?
        };
        drop(signals);

        let writer_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(format!("{THREAD_NAME}-set"))
            .spawn(move || serve_commands(&writer_shared, &session, &clipboard, &queued))
            .map_err(ClipboardError::PortalSpawn)?;

        Ok(Self { shared, commands })
    }

    pub fn link(&self, max_bytes: usize, watch: bool) -> Box<dyn ClipboardBackend> {
        let (changes_sender, changes) = mpsc::channel();
        let mut next = self.shared.next_token.lock();
        *next += 1;
        let token = *next;
        drop(next);

        *self.shared.subscriber.lock() = Some(Subscriber {
            changes: changes_sender,
            watch,
            max_bytes,
            token,
        });

        Box::new(PortalLink {
            shared: Arc::clone(&self.shared),
            commands: self.commands.clone(),
            changes,
            token,
        })
    }
}

struct PortalLink {
    shared: Arc<Shared>,
    commands: Sender<String>,
    changes: Receiver<String>,
    token: u64,
}

impl ClipboardBackend for PortalLink {
    fn kind(&self) -> BackendKind {
        BackendKind::Portal
    }

    fn poll_change(&mut self) -> Result<Option<String>, ClipboardError> {
        if !self.shared.alive.load(Ordering::Relaxed) {
            return Err(ClipboardError::BackendStopped);
        }

        Ok(self.changes.try_recv().ok())
    }

    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        if !self.shared.alive.load(Ordering::Relaxed) {
            return Err(ClipboardError::BackendStopped);
        }

        self.commands
            .send(text.to_owned())
            .map_err(|_| ClipboardError::BackendStopped)
    }
}

impl Drop for PortalLink {
    fn drop(&mut self) {
        let mut subscriber = self.shared.subscriber.lock();
        if subscriber
            .as_ref()
            .is_some_and(|current| current.token == self.token)
        {
            *subscriber = None;
        }
    }
}

fn serve_commands(
    shared: &Shared,
    session: &RemoteDesktopSession,
    clipboard: &Clipboard,
    queued: &Receiver<String>,
) {
    while let Ok(text) = queued.recv() {
        *shared.current.lock() = Some(Arc::from(text.as_str()));

        let options = SetSelectionOptions::default().set_mime_types(&[TEXT_MIME]);
        let result = pollster::block_on(clipboard.set_selection(session.session(), options))
            .map_err(portal(PortalStep::SetSelection));
        if result.is_err() {
            shared.alive.store(false, Ordering::Relaxed);
            return;
        }
    }
}

async fn listen(
    shared: &Arc<Shared>,
    session: &Arc<RemoteDesktopSession>,
    clipboard: &Arc<Clipboard>,
) -> Result<(), ClipboardError> {
    let owner = clipboard
        .receive_selection_owner_changed::<ashpd::desktop::remote_desktop::RemoteDesktop>()
        .await
        .map_err(portal(PortalStep::Subscribe))?
        .map(|(origin, change)| {
            (
                format!("{origin:?}"),
                Signal::Owner {
                    is_ours: change.session_is_owner().unwrap_or(false),
                    mimes: change.mime_types().to_vec(),
                },
            )
        });
    let transfer = clipboard
        .receive_selection_transfer::<ashpd::desktop::remote_desktop::RemoteDesktop>()
        .await
        .map_err(portal(PortalStep::Subscribe))?
        .map(|(origin, mime, serial)| (format!("{origin:?}"), Signal::Transfer { mime, serial }));

    let ours = format!("{:?}", session.session());
    let mut signals = Box::pin(stream::select(owner, transfer));

    while let Some((origin, signal)) = signals.next().await {
        if origin != ours {
            continue;
        }

        match signal {
            Signal::Owner { is_ours, mimes } => {
                read_selection(shared, session, clipboard, is_ours, &mimes).await;
            }
            Signal::Transfer { mime, serial } => {
                write_selection(shared, session, clipboard, &mime, serial).await;
            }
        }
    }

    Ok(())
}

async fn read_selection(
    shared: &Arc<Shared>,
    session: &RemoteDesktopSession,
    clipboard: &Clipboard,
    is_ours: bool,
    mimes: &[String],
) {
    let wanted = {
        let subscriber = shared.subscriber.lock();
        subscriber
            .as_ref()
            .filter(|subscriber| subscriber.watch)
            .map(|subscriber| (subscriber.changes.clone(), subscriber.max_bytes))
    };
    let Some((changes, max_bytes)) = wanted else {
        return;
    };
    let shareable = mimes.iter().any(|mime| mime == TEXT_MIME)
        && !mimes.iter().any(|mime| mime == PASSWORD_HINT_MIME);
    if is_ours || !shareable {
        return;
    }

    let Ok(fd) = clipboard.selection_read(session.session(), TEXT_MIME).await else {
        return;
    };
    let fd = OwnedFd::from(fd);

    thread::spawn(move || {
        if let Some(text) = read_to_end(fd, max_bytes) {
            changes.send(text).ok();
        }
    });
}

fn read_to_end(fd: OwnedFd, max_bytes: usize) -> Option<String> {
    let mut job = ReadJob::from_fd(fd, max_bytes, Instant::now()).ok()?;
    let limit = Timespec {
        tv_sec: 0,
        tv_nsec: i64::from(READ_POLL.subsec_nanos()),
    };

    loop {
        match job.advance(Instant::now()) {
            ReadStep::Done => return job.into_text(),
            ReadStep::Abandoned => return None,
            ReadStep::Pending => {
                let mut fds = [PollFd::new(job.fd(), PollFlags::IN)];
                poll(&mut fds, Some(&limit)).ok();
            }
        }
    }
}

async fn write_selection(
    shared: &Arc<Shared>,
    session: &Arc<RemoteDesktopSession>,
    clipboard: &Arc<Clipboard>,
    mime: &str,
    serial: u32,
) {
    let text = shared.current.lock().clone().filter(|_| mime == TEXT_MIME);
    let Ok(fd) = clipboard.selection_write(session.session(), serial).await else {
        return;
    };
    let fd = OwnedFd::from(fd);

    let (session, clipboard) = (Arc::clone(session), Arc::clone(clipboard));
    thread::spawn(move || {
        let written =
            text.is_some_and(|text| write_all(&fd, text.as_bytes(), TRANSFER_TIMEOUT).is_ok());
        drop(fd);
        pollster::block_on(clipboard.selection_write_done(session.session(), serial, written)).ok();
    });
}
