mod handlers;

use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use parking_lot::Mutex;
use rusplay_config::PointerMode;
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    io::Errno,
};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::{
    Connection, EventQueue, Proxy, globals::registry_queue_init, protocol::wl_surface::WlSurface,
};

use crate::{
    InputError, InputEvent,
    capture::{ActivePointer, CaptureCore, CaptureOptions, StreamRegion},
    wayland::handlers::{Command, State},
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const THREAD_NAME: &str = "rusplay-wayland-input";

#[derive(Debug, Clone, Copy)]
pub struct WaylandTarget {
    pub display: NonNull<c_void>,
    pub surface: NonNull<c_void>,
}

#[derive(Debug, Clone)]
pub struct HostCursorObserver {
    commands: Sender<Command>,
}

impl HostCursorObserver {
    pub fn observe(&self, visible: bool) {
        self.commands.send(Command::HostCursor { visible }).ok();
    }
}

pub struct WaylandCapture {
    core: Arc<Mutex<CaptureCore>>,
    commands: Sender<Command>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), InputError>>>,
}

impl WaylandCapture {
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn attach(
        target: WaylandTarget,
        options: CaptureOptions,
        sink: impl FnMut(InputEvent) + Send + 'static,
    ) -> Result<Self, InputError> {
        let backend = unsafe { Backend::from_foreign_display(target.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let surface_id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), target.surface.as_ptr().cast()) }
                .map_err(|_| InputError::WaylandObjectInvalid {
                object: "wl_surface",
            })?;
        let surface = WlSurface::from_id(&connection, surface_id).map_err(|_| {
            InputError::WaylandObjectInvalid {
                object: "wl_surface",
            }
        })?;

        let (globals, mut queue) = registry_queue_init::<State>(&connection)
            .map_err(|error| InputError::WaylandGlobals(Box::new(error)))?;
        let core = Arc::new(Mutex::new(CaptureCore::new(options)));
        let mut state = State::bind(
            &globals,
            &queue.handle(),
            surface,
            Arc::clone(&core),
            Box::new(sink),
        )?;

        queue
            .roundtrip(&mut state)
            .map_err(|error| InputError::WaylandDispatch(Box::new(error)))?;
        queue
            .roundtrip(&mut state)
            .map_err(|error| InputError::WaylandDispatch(Box::new(error)))?;

        let (commands, queued) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || serve(&connection, queue, state, &queued, &worker_stop))
            .map_err(InputError::WaylandSpawn)?;

        Ok(Self {
            core,
            commands,
            stop,
            thread: Some(thread),
        })
    }

    pub fn set_region(&self, region: Option<StreamRegion>) {
        self.core.lock().set_region(region);
    }

    pub fn is_engaged(&self) -> bool {
        self.core.lock().is_engaged()
    }

    pub fn engage(&self) {
        self.commands.send(Command::Engage).ok();
    }

    pub fn release(&self) {
        self.commands.send(Command::Release).ok();
    }

    pub fn set_pointer_mode(&self, mode: PointerMode) {
        self.commands.send(Command::SetMode(mode)).ok();
    }

    pub fn cycle_pointer_mode(&self) {
        self.commands.send(Command::CycleMode).ok();
    }

    pub fn pointer_mode(&self) -> PointerMode {
        self.core.lock().mode()
    }

    pub fn active_pointer(&self) -> ActivePointer {
        self.core.lock().active_pointer()
    }

    pub fn host_cursor_observer(&self) -> HostCursorObserver {
        HostCursorObserver {
            commands: self.commands.clone(),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn stop(mut self) -> Result<(), InputError> {
        self.shut_down()
    }

    fn shut_down(&mut self) -> Result<(), InputError> {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .unwrap_or(Err(InputError::WaylandThreadPanicked)),
            None => Ok(()),
        }
    }
}

impl Drop for WaylandCapture {
    fn drop(&mut self) {
        self.shut_down().ok();
    }
}

fn serve(
    connection: &Connection,
    mut queue: EventQueue<State>,
    mut state: State,
    commands: &Receiver<Command>,
    stop: &AtomicBool,
) -> Result<(), InputError> {
    while !stop.load(Ordering::Relaxed) {
        while let Ok(command) = commands.try_recv() {
            state.command(command);
        }
        state.tick();

        queue
            .dispatch_pending(&mut state)
            .map_err(|error| InputError::WaylandDispatch(Box::new(error)))?;
        connection
            .flush()
            .map_err(|error| InputError::WaylandRead(Box::new(error)))?;

        let Some(guard) = connection.prepare_read() else {
            continue;
        };

        let limit = Timespec {
            tv_sec: 0,
            tv_nsec: i64::from(POLL_INTERVAL.subsec_nanos()),
        };
        let ready = {
            let socket = guard.connection_fd();
            let mut fds = [PollFd::new(&socket, PollFlags::IN)];
            match poll(&mut fds, Some(&limit)) {
                Ok(count) => count > 0,
                Err(Errno::INTR) => false,
                Err(errno) => return Err(InputError::WaylandPoll(errno.into())),
            }
        };

        if ready {
            guard
                .read()
                .map_err(|error| InputError::WaylandRead(Box::new(error)))?;
        }
    }

    state.shutdown();
    connection
        .flush()
        .map_err(|error| InputError::WaylandRead(Box::new(error)))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use rusplay_config::{InputConfig, PointerMode};
    use wayland_client::{
        Dispatch, Proxy, QueueHandle,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_surface::WlSurface},
    };

    use super::*;

    struct Probe;

    impl Dispatch<WlRegistry, GlobalListContents> for Probe {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WlCompositor, ()> for Probe {
        fn event(
            _: &mut Self,
            _: &WlCompositor,
            _: <WlCompositor as Proxy>::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WlSurface, ()> for Probe {
        fn event(
            _: &mut Self,
            _: &WlSurface,
            _: <WlSurface as Proxy>::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    struct LiveSurface {
        capture: Option<WaylandCapture>,
        _surface: WlSurface,
        _queue: EventQueue<Probe>,
        _connection: Connection,
    }

    impl LiveSurface {
        fn attach(mode: PointerMode) -> Self {
            let connection = Connection::connect_to_env().unwrap();
            let (globals, mut queue) = registry_queue_init::<Probe>(&connection).unwrap();
            let compositor: WlCompositor = globals.bind(&queue.handle(), 1..=4, ()).unwrap();
            let surface = compositor.create_surface(&queue.handle(), ());
            queue.roundtrip(&mut Probe).unwrap();

            let display = NonNull::new(connection.backend().display_ptr().cast()).unwrap();
            let surface_ptr = NonNull::new(surface.id().as_ptr().cast()).unwrap();
            let options = CaptureOptions::from(&InputConfig {
                pointer_mode: mode,
                ..InputConfig::default()
            });
            let (sender, _events) = channel();
            let capture = unsafe {
                WaylandCapture::attach(
                    WaylandTarget {
                        display,
                        surface: surface_ptr,
                    },
                    options,
                    move |event| {
                        sender.send(event).ok();
                    },
                )
            }
            .unwrap();

            Self {
                capture: Some(capture),
                _surface: surface,
                _queue: queue,
                _connection: connection,
            }
        }

        fn capture(&self) -> &WaylandCapture {
            self.capture.as_ref().unwrap()
        }

        fn stop(&mut self) -> Result<(), InputError> {
            self.capture.take().unwrap().stop()
        }
    }

    #[test]
    #[ignore = "needs a running wayland compositor with pointer constraints"]
    fn live_relative_capture_binds_the_globals_and_stops_cleanly() {
        let mut live = LiveSurface::attach(PointerMode::Relative);

        live.capture().engage();
        thread::sleep(Duration::from_millis(100));
        assert!(!live.capture().is_finished());
        assert!(live.capture().is_engaged());

        live.capture().release();
        thread::sleep(Duration::from_millis(100));
        assert!(!live.capture().is_engaged());
        live.stop().unwrap();
    }

    #[test]
    #[ignore = "needs a running wayland compositor with pointer constraints"]
    fn live_pointer_mode_switches_while_engaged_without_a_second_lock() {
        let mut live = LiveSurface::attach(PointerMode::Relative);

        live.capture().engage();
        live.capture().set_pointer_mode(PointerMode::Absolute);
        live.capture().set_pointer_mode(PointerMode::Relative);
        live.capture().cycle_pointer_mode();
        thread::sleep(Duration::from_millis(100));
        assert!(!live.capture().is_finished());
        assert_eq!(live.capture().pointer_mode(), PointerMode::Auto);

        live.capture().release();
        live.stop().unwrap();
    }

    #[test]
    #[ignore = "needs a running wayland compositor"]
    fn live_absolute_capture_binds_and_stops_cleanly() {
        let mut live = LiveSurface::attach(PointerMode::Absolute);

        live.capture().set_region(Some(StreamRegion {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 360.0,
        }));
        thread::sleep(Duration::from_millis(100));
        assert!(!live.capture().is_finished());
        live.stop().unwrap();
    }
}
