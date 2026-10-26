use std::{ffi::c_void, ops::RangeInclusive, ptr::NonNull, sync::Arc};

use parking_lot::Mutex;
use rusplay_input::wayland::WaylandTarget;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::{Backend, ObjectId},
    globals::{BindError, GlobalList, GlobalListContents, registry_queue_init},
    protocol::{
        wl_compositor::WlCompositor,
        wl_output::WlOutput,
        wl_region::WlRegion,
        wl_registry::WlRegistry,
        wl_subcompositor::WlSubcompositor,
        wl_subsurface::WlSubsurface,
        wl_surface::{self, WlSurface},
    },
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::ClientError;

const COMPOSITOR_VERSIONS: RangeInclusive<u32> = 1..=4;
const SINGLE_VERSION: RangeInclusive<u32> = 1..=1;

pub(crate) struct ChildSurface {
    connection: Connection,
    queue: Mutex<EventQueue<SurfaceEvents>>,
    entered: Arc<Mutex<Vec<WlOutput>>>,
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
}

impl ChildSurface {
    pub(crate) unsafe fn attach(target: WaylandTarget) -> Result<Self, ClientError> {
        let backend = unsafe { Backend::from_foreign_display(target.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let parent_id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), target.surface.as_ptr().cast()) }
                .map_err(|_| ClientError::WindowHandleInvalid)?;
        let parent = WlSurface::from_id(&connection, parent_id)
            .map_err(|_| ClientError::WindowHandleInvalid)?;

        let (globals, mut queue) = registry_queue_init::<SurfaceEvents>(&connection)
            .map_err(ClientError::WaylandRegistry)?;
        let handle = queue.handle();
        let compositor: WlCompositor =
            bind(&globals, &handle, COMPOSITOR_VERSIONS, "wl_compositor")?;
        let subcompositor: WlSubcompositor =
            bind(&globals, &handle, SINGLE_VERSION, "wl_subcompositor")?;
        let viewporter: WpViewporter = bind(&globals, &handle, SINGLE_VERSION, "wp_viewporter")?;

        let surface = compositor.create_surface(&handle, ());
        let subsurface = subcompositor.get_subsurface(&surface, &parent, &handle, ());
        let viewport = viewporter.get_viewport(&surface, &handle, ());
        let no_input = compositor.create_region(&handle, ());

        subsurface.set_desync();
        subsurface.place_above(&parent);
        surface.set_input_region(Some(&no_input));
        surface.commit();
        no_input.destroy();
        viewporter.destroy();
        subcompositor.destroy();

        let entered = Arc::new(Mutex::new(Vec::new()));

        queue
            .roundtrip(&mut SurfaceEvents::new(&entered))
            .map_err(ClientError::WindowDispatch)?;

        Ok(Self {
            connection,
            queue: Mutex::new(queue),
            entered,
            surface,
            subsurface,
            viewport,
        })
    }

    pub(crate) fn display(&self) -> Option<NonNull<c_void>> {
        NonNull::new(self.connection.backend().display_ptr().cast())
    }

    pub(crate) fn surface(&self) -> Option<NonNull<c_void>> {
        NonNull::new(self.surface.id().as_ptr().cast())
    }

    pub(crate) fn set_position(&self, x: i32, y: i32) -> Result<(), ClientError> {
        self.subsurface.set_position(x, y);
        self.dispatch()
    }

    pub(crate) fn set_destination(&self, width: u32, height: u32) {
        self.viewport.set_destination(
            i32::try_from(width).unwrap_or(i32::MAX),
            i32::try_from(height).unwrap_or(i32::MAX),
        );
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(crate) fn entered_outputs(&self) -> Vec<WlOutput> {
        self.entered.lock().clone()
    }

    pub(crate) fn dispatch(&self) -> Result<(), ClientError> {
        self.queue
            .lock()
            .dispatch_pending(&mut SurfaceEvents::new(&self.entered))
            .map_err(ClientError::WindowDispatch)?;

        Ok(())
    }
}

impl Drop for ChildSurface {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
        self.connection.flush().ok();
    }
}

fn bind<I>(
    globals: &GlobalList,
    handle: &QueueHandle<SurfaceEvents>,
    versions: RangeInclusive<u32>,
    interface: &'static str,
) -> Result<I, ClientError>
where
    I: Proxy + 'static,
    SurfaceEvents: Dispatch<I, ()>,
{
    globals
        .bind(handle, versions, ())
        .map_err(|source: BindError| ClientError::WindowGlobalMissing { interface, source })
}

pub(crate) struct SurfaceEvents {
    entered: Arc<Mutex<Vec<WlOutput>>>,
}

impl SurfaceEvents {
    fn new(entered: &Arc<Mutex<Vec<WlOutput>>>) -> Self {
        Self {
            entered: Arc::clone(entered),
        }
    }
}

impl Dispatch<WlSurface, ()> for SurfaceEvents {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut entered = state.entered.lock();

        match event {
            wl_surface::Event::Enter { output } if !entered.contains(&output) => {
                entered.push(output);
            }
            wl_surface::Event::Leave { output } => entered.retain(|held| *held != output),
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for SurfaceEvents {
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

macro_rules! ignore_events {
    ($($interface:ty),* $(,)?) => {
        $(
            impl Dispatch<$interface, ()> for SurfaceEvents {
                fn event(
                    _: &mut Self,
                    _: &$interface,
                    _: <$interface as Proxy>::Event,
                    (): &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
}

ignore_events!(
    WlCompositor,
    WlSubcompositor,
    WlSubsurface,
    WlRegion,
    WpViewporter,
    WpViewport,
);
