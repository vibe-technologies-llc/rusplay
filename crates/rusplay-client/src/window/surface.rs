use std::{
    collections::VecDeque,
    ffi::c_void,
    ops::RangeInclusive,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_input::wayland::WaylandTarget;
use rustix::time::{ClockId, clock_gettime};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
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
use wayland_protocols::wp::{
    presentation_time::client::{
        wp_presentation::{self, WpPresentation},
        wp_presentation_feedback::{self, WpPresentationFeedback},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

use crate::{
    ClientError,
    window::pacing::{Feedback, PresentationFlags, Presented, Submission, instant_of},
};

const COMPOSITOR_VERSIONS: RangeInclusive<u32> = 1..=4;
const SINGLE_VERSION: RangeInclusive<u32> = 1..=1;
const PRESENTATION_VERSIONS: RangeInclusive<u32> = 1..=2;
const UNKNOWN_CLOCK: u32 = u32::MAX;
const MAX_FEEDBACK: usize = 64;

#[derive(Debug)]
struct FeedbackLog {
    clock: AtomicU32,
    outcomes: Mutex<VecDeque<Feedback>>,
}

impl FeedbackLog {
    fn new() -> Self {
        Self {
            clock: AtomicU32::new(UNKNOWN_CLOCK),
            outcomes: Mutex::new(VecDeque::new()),
        }
    }

    fn push(&self, outcome: Feedback) {
        let mut outcomes = self.outcomes.lock();
        if outcomes.len() == MAX_FEEDBACK {
            outcomes.pop_front();
        }
        outcomes.push_back(outcome);
    }

    fn clock_now(&self) -> Option<Duration> {
        let clock = match self.clock.load(Ordering::Relaxed) {
            0 => ClockId::Realtime,
            1 => ClockId::Monotonic,
            4 => ClockId::MonotonicRaw,
            7 => ClockId::Boottime,
            _ => return None,
        };
        let now = clock_gettime(clock);

        Some(Duration::new(
            u64::try_from(now.tv_sec).ok()?,
            u32::try_from(now.tv_nsec).ok()?,
        ))
    }
}

pub(crate) struct ChildSurface {
    connection: Connection,
    queue: Mutex<EventQueue<SurfaceEvents>>,
    entered: Arc<Mutex<Vec<WlOutput>>>,
    feedback: Arc<FeedbackLog>,
    handle: QueueHandle<SurfaceEvents>,
    presentation: Option<WpPresentation>,
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
        let presentation: Option<WpPresentation> =
            globals.bind(&handle, PRESENTATION_VERSIONS, ()).ok();

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
        let feedback = Arc::new(FeedbackLog::new());

        queue
            .roundtrip(&mut SurfaceEvents::new(&entered, &feedback))
            .map_err(ClientError::WindowDispatch)?;

        Ok(Self {
            connection,
            queue: Mutex::new(queue),
            entered,
            feedback,
            handle,
            presentation,
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

    pub(crate) fn has_presentation_feedback(&self) -> bool {
        self.presentation.is_some()
    }

    pub(crate) fn request_feedback(&self, submission: Submission) {
        if let Some(presentation) = &self.presentation {
            presentation.feedback(&self.surface, &self.handle, submission);
        }
    }

    pub(crate) fn take_feedback(&self) -> Vec<Feedback> {
        self.feedback.outcomes.lock().drain(..).collect()
    }

    pub(crate) fn unmap(&self) {
        self.surface.attach(None, 0, 0);
        self.surface.commit();
        self.connection.flush().ok();
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
            .dispatch_pending(&mut SurfaceEvents::new(&self.entered, &self.feedback))
            .map_err(ClientError::WindowDispatch)?;

        Ok(())
    }
}

impl Drop for ChildSurface {
    fn drop(&mut self) {
        if let Some(presentation) = &self.presentation {
            presentation.destroy();
        }
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
    feedback: Arc<FeedbackLog>,
}

impl SurfaceEvents {
    fn new(entered: &Arc<Mutex<Vec<WlOutput>>>, feedback: &Arc<FeedbackLog>) -> Self {
        Self {
            entered: Arc::clone(entered),
            feedback: Arc::clone(feedback),
        }
    }
}

impl Dispatch<WpPresentation, ()> for SurfaceEvents {
    fn event(
        state: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.feedback.clock.store(clk_id, Ordering::Relaxed);
        }
    }
}

impl Dispatch<WpPresentationFeedback, Submission> for SurfaceEvents {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        submission: &Submission,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let outcome = match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                flags,
                ..
            } => {
                let Some(clock_now) = state.feedback.clock_now() else {
                    return;
                };
                let stamp = Duration::new(
                    (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo),
                    tv_nsec.min(999_999_999),
                );
                Feedback::Presented(Presented {
                    submission: *submission,
                    shown_at: instant_of(stamp, clock_now, Instant::now()),
                    refresh: (refresh > 0).then(|| Duration::from_nanos(u64::from(refresh))),
                    flags: PresentationFlags::from_bits(match flags {
                        WEnum::Value(kind) => kind.bits(),
                        WEnum::Unknown(bits) => bits,
                    }),
                })
            }
            wp_presentation_feedback::Event::Discarded => Feedback::Discarded(*submission),
            _ => return,
        };

        state.feedback.push(outcome);
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

#[cfg(test)]
mod tests {
    use wayland_client::globals::registry_queue_init;

    use super::*;

    const ROUNDTRIPS: usize = 8;

    #[test]
    #[ignore = "needs a running wayland compositor and a Vulkan GPU"]
    fn resizing_while_frames_are_submitted_keeps_the_surface_configured() {
        let connection = Connection::connect_to_env().unwrap();
        let (globals, mut queue) = registry_queue_init::<SurfaceEvents>(&connection).unwrap();
        let handle = queue.handle();
        let compositor: WlCompositor = globals.bind(&handle, COMPOSITOR_VERSIONS, ()).unwrap();
        let native = compositor.create_surface(&handle, ());
        let entered = Arc::new(Mutex::new(Vec::new()));
        let feedback = Arc::new(FeedbackLog::new());
        queue
            .roundtrip(&mut SurfaceEvents::new(&entered, &feedback))
            .unwrap();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(wgpu::rwh::RawDisplayHandle::Wayland(
                    wgpu::rwh::WaylandDisplayHandle::new(
                        NonNull::new(connection.backend().display_ptr().cast()).unwrap(),
                    ),
                )),
                raw_window_handle: wgpu::rwh::RawWindowHandle::Wayland(
                    wgpu::rwh::WaylandWindowHandle::new(
                        NonNull::new(native.id().as_ptr().cast()).unwrap(),
                    ),
                ),
            })
        }
        .unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..wgpu::RequestAdapterOptions::default()
        }))
        .unwrap();
        let (device, gpu_queue) =
            pollster::block_on(adapter.request_device(&rusplay_scale::device_descriptor(&adapter)))
                .unwrap();
        let gpu = crate::Gpu::new(device, gpu_queue);
        let mut config = surface.get_default_config(&adapter, 640, 360).unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let started = std::sync::Barrier::new(2);

        std::thread::scope(|threads| {
            let worker = threads.spawn(|| {
                let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("resize-race-workload"),
                    size: 8 * 1024 * 1024,
                    usage: wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                started.wait();
                while !stop.load(Ordering::Relaxed) {
                    gpu.guard(|| {
                        let mut encoder = gpu
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                        encoder.clear_buffer(&buffer, 0, None);
                        gpu.queue.submit([encoder.finish()]);
                        Ok(())
                    })
                    .unwrap();
                }
            });
            started.wait();
            let result = (|| {
                for side in (64..164).cycle().take(200) {
                    config.width = side * 2;
                    config.height = side;
                    gpu.guard(|| {
                        gpu.configure_surface(&surface, &config)?;
                        match surface.get_current_texture() {
                            wgpu::CurrentSurfaceTexture::Success(frame)
                            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => drop(frame),
                            wgpu::CurrentSurfaceTexture::Validation => {
                                return Err(ClientError::SurfaceValidation);
                            }
                            _ => {}
                        }
                        Ok(())
                    })?;
                }
                Ok::<_, ClientError>(())
            })();
            stop.store(true, Ordering::Relaxed);
            worker.join().unwrap();

            result.unwrap();
        });
    }

    #[test]
    #[ignore = "needs a running wayland compositor with wp_presentation"]
    fn feedback_for_a_commit_comes_back_through_the_surface_queue() {
        let connection = Connection::connect_to_env().unwrap();
        let (globals, mut queue) = registry_queue_init::<SurfaceEvents>(&connection).unwrap();
        let handle = queue.handle();
        let compositor: WlCompositor = globals.bind(&handle, COMPOSITOR_VERSIONS, ()).unwrap();
        let Ok(presentation) =
            globals.bind::<WpPresentation, _, _>(&handle, PRESENTATION_VERSIONS, ())
        else {
            return;
        };
        let advertised = globals.contents().clone_list();

        let entered = Arc::new(Mutex::new(Vec::new()));
        let feedback = Arc::new(FeedbackLog::new());
        let mut state = SurfaceEvents::new(&entered, &feedback);
        let surface = compositor.create_surface(&handle, ());
        let now = Instant::now();

        queue.roundtrip(&mut state).unwrap();
        presentation.feedback(
            &surface,
            &handle,
            Submission {
                submitted_at: now,
                captured_at: None,
                target: None,
            },
        );
        surface.commit();
        surface.destroy();
        for _ in 0..ROUNDTRIPS {
            queue.roundtrip(&mut state).unwrap();
            if !feedback.outcomes.lock().is_empty() {
                break;
            }
        }

        let outcomes = feedback.outcomes.lock().clone();
        let staging = ["wp_commit_timing_manager_v1", "wp_fifo_manager_v1"].map(|name| {
            (
                name,
                advertised.iter().any(|global| global.interface == name),
            )
        });
        eprintln!("{outcomes:?} {staging:?}");
        assert!(feedback.clock_now().is_some());
        assert_eq!(outcomes.len(), 1);

        presentation.destroy();
        connection.flush().unwrap();
    }
}
