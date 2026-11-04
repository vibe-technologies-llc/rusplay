use std::{
    collections::HashMap,
    fs::File,
    os::{fd::AsFd, unix::fs::FileExt},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rustix::event::{PollFd, PollFlags, poll};
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_output::{self, WlOutput},
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

use super::stack::Stack;

const EDGE: u32 = 256;
const BYTES_PER_PIXEL: u32 = 4;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const PRIMARY_OUTPUT: &str = "Virtual-0";
const TITLE: &str = "rusplay nested painter";

struct Painting {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    base: Option<XdgWmBase>,
    surface: Option<WlSurface>,
    buffer: Option<WlBuffer>,
    file: File,
    outputs: Vec<(u32, WlOutput)>,
    names: HashMap<u32, String>,
    configured: bool,
    callback_pending: bool,
    painted: u64,
}

impl Dispatch<WlRegistry, ()> for Painting {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };

        match interface.as_str() {
            "wl_compositor" => state.compositor = Some(registry.bind(name, 4, queue, ())),
            "wl_output" => {
                let output = registry.bind(name, version.min(4), queue, name);
                state.outputs.push((name, output));
            }
            "wl_shm" => state.shm = Some(registry.bind(name, 1, queue, ())),
            "xdg_wm_base" => state.base = Some(registry.bind(name, version.min(5), queue, ())),
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Painting {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Painting {
    fn event(
        state: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<WlCallback, ()> for Painting {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.callback_pending = false;
        }
    }
}

impl Dispatch<WlOutput, u32> for Painting {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.names.insert(*global, name);
        }
    }
}

delegate_noop!(Painting: WlCompositor);
delegate_noop!(Painting: ignore WlShm);
delegate_noop!(Painting: WlShmPool);
delegate_noop!(Painting: ignore WlBuffer);
delegate_noop!(Painting: ignore WlSurface);
delegate_noop!(Painting: ignore XdgToplevel);

impl Painting {
    fn paint(&mut self, queue: &QueueHandle<Self>) {
        let count = self.painted;
        self.painted += 1;
        let shade = u8::try_from(count % 200).unwrap() + 40;
        let pixel = u32::from_le_bytes([shade, 255 - shade, shade / 2, 0xff]);
        let row: Vec<u8> = std::iter::repeat_n(pixel.to_le_bytes(), EDGE as usize)
            .flatten()
            .collect();
        for line in 0..EDGE {
            self.file
                .write_all_at(&row, u64::from(line * EDGE * BYTES_PER_PIXEL))
                .unwrap();
        }

        let surface = self.surface.as_ref().unwrap();
        surface.attach(self.buffer.as_ref(), 0, 0);
        surface.damage_buffer(
            0,
            0,
            i32::try_from(EDGE).unwrap(),
            i32::try_from(EDGE).unwrap(),
        );
        surface.frame(queue, ());
        surface.commit();
        self.callback_pending = true;
    }
}

pub struct Painter {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Painter {
    pub fn start(stack: &Stack, animate: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stream = stack.connect_wayland();

        let thread = thread::Builder::new()
            .name("nested-painter".into())
            .spawn({
                let stop = Arc::clone(&stop);
                move || run(stream, animate, &stop)
            })
            .unwrap();

        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

fn run(stream: std::os::unix::net::UnixStream, animate: bool, stop: &AtomicBool) {
    let connection = Connection::from_socket(stream).unwrap();
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    let _registry = connection.display().get_registry(&handle, ());

    let file = tempfile::tempfile().unwrap();
    file.set_len(u64::from(EDGE * EDGE * BYTES_PER_PIXEL))
        .unwrap();
    let mut state = Painting {
        compositor: None,
        shm: None,
        base: None,
        surface: None,
        buffer: None,
        file,
        outputs: Vec::new(),
        names: HashMap::new(),
        configured: false,
        callback_pending: false,
        painted: 0,
    };
    queue.roundtrip(&mut state).unwrap();

    let compositor = state.compositor.clone().unwrap();
    let shm = state.shm.clone().unwrap();
    let base = state.base.clone().unwrap();

    let edge = i32::try_from(EDGE).unwrap();
    let pool = shm.create_pool(
        state.file.as_fd(),
        i32::try_from(EDGE * EDGE * BYTES_PER_PIXEL).unwrap(),
        &handle,
        (),
    );
    state.buffer = Some(pool.create_buffer(
        0,
        edge,
        edge,
        edge * 4,
        wl_shm::Format::Xrgb8888,
        &handle,
        (),
    ));

    let surface = compositor.create_surface(&handle, ());
    let shell = base.get_xdg_surface(&surface, &handle, ());
    let toplevel = shell.get_toplevel(&handle, ());
    toplevel.set_title(TITLE.into());
    queue.roundtrip(&mut state).unwrap();
    let target = state
        .outputs
        .iter()
        .find(|(global, _)| {
            state
                .names
                .get(global)
                .is_some_and(|name| name != PRIMARY_OUTPUT)
        })
        .map(|(_, output)| output.clone());
    eprintln!(
        "nested outputs: {:?}",
        state.names.values().collect::<Vec<_>>()
    );
    toplevel.set_fullscreen(target.as_ref());
    surface.commit();
    state.surface = Some(surface);

    while !state.configured && !stop.load(Ordering::Relaxed) {
        queue.blocking_dispatch(&mut state).unwrap();
    }

    while !stop.load(Ordering::Relaxed) {
        queue.dispatch_pending(&mut state).unwrap();
        if animate && !state.callback_pending {
            state.paint(&handle);
        }
        connection.flush().unwrap();

        if let Some(guard) = connection.prepare_read() {
            let descriptor = guard.connection_fd();
            let mut fds = [PollFd::new(&descriptor, PollFlags::IN)];
            let timeout = rustix::event::Timespec {
                tv_sec: 0,
                tv_nsec: i64::try_from(POLL_INTERVAL.as_nanos()).unwrap(),
            };
            if poll(&mut fds, Some(&timeout)).unwrap_or(0) > 0 {
                guard.read().ok();
            }
        }
    }

    toplevel.destroy();
    shell.destroy();
    connection.flush().ok();
}
