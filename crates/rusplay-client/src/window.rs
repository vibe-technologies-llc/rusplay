mod choice;
mod compose;
mod cursor;
mod dither;
mod follow;
mod layout;
mod output;
mod surface;

use std::{
    sync::Arc,
    thread::{self, JoinHandle},
};

use parking_lot::{Condvar, Mutex};
use rusplay_config::{
    Config, DisplayOutputConfig, DisplayTargetConfig, InputConfig, PointerMode, PresentMode,
};
use rusplay_core::Resolution;
pub use rusplay_input::wayland::WaylandTarget;
use rusplay_input::{
    capture::{ActivePointer, CaptureOptions, ClientAction},
    wayland::{HostCursorObserver, WaylandCapture},
};
use rusplay_scale::ColorVolume;

pub use crate::window::layout::{LogicalArea, WindowLayout};
use crate::{
    ClientError, ClientStream, DisplayImage, FrameSink, Gpu, LatestCursor, PresentedFrame, Stage,
    cursor::CursorWatcher,
    display::{query_display, resolve_target},
    window::{
        choice::{Admission, OutputChoice},
        compose::Composer,
        cursor::CursorLayer,
        follow::DisplayFollower,
        output::{Display, SurfaceOutput},
        surface::ChildSurface,
    },
};

#[derive(Debug, Default)]
struct Pending {
    frame: Option<PresentedFrame>,
    layout: Option<WindowLayout>,
    cursor: Option<LatestCursor>,
    display: Option<DisplayImage>,
    dirty: bool,
    stop: bool,
}

#[derive(Debug)]
struct Shared {
    pending: Mutex<Pending>,
    wake: Condvar,
    color_target: Mutex<ColorVolume>,
}

impl Shared {
    fn new(color_target: ColorVolume) -> Self {
        Self {
            pending: Mutex::new(Pending::default()),
            wake: Condvar::new(),
            color_target: Mutex::new(color_target),
        }
    }

    fn update<R>(&self, change: impl FnOnce(&mut Pending) -> R) -> R {
        let mut pending = self.pending.lock();
        let result = change(&mut pending);
        pending.dirty = true;
        drop(pending);
        self.wake.notify_one();

        result
    }
}

impl CursorWatcher for Shared {
    fn changed(&self) {
        self.update(|_| ());
    }
}

#[derive(Debug)]
struct PointerFollower {
    cursor: LatestCursor,
    observer: HostCursorObserver,
    last_visible: Mutex<Option<bool>>,
}

impl CursorWatcher for PointerFollower {
    fn changed(&self) {
        let Some(visible) = self.cursor.latest().map(|cursor| cursor.visible) else {
            return;
        };

        let mut last = self.last_visible.lock();
        if last.replace(visible) != Some(visible) {
            self.observer.observe(visible);
        }
    }
}

#[derive(Debug, Clone)]
pub struct WindowSink {
    shared: Arc<Shared>,
}

impl FrameSink for WindowSink {
    fn present(&mut self, frame: PresentedFrame) {
        let replaced = self.shared.update(|pending| pending.frame.replace(frame));

        drop(replaced);
    }

    fn color_target(&self) -> Option<ColorVolume> {
        Some(*self.shared.color_target.lock())
    }
}

pub struct StreamWindow {
    target: WaylandTarget,
    capture: Option<WaylandCapture>,
    gpu: Gpu,
    shared: Arc<Shared>,
    surface: Arc<ChildSurface>,
    presenter: Option<JoinHandle<Result<(), ClientError>>>,
    layout: Option<WindowLayout>,
    follower: Option<DisplayFollower>,
}

impl StreamWindow {
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn attach(target: WaylandTarget, config: &Config) -> Result<Self, ClientError> {
        let child = Arc::new(unsafe { ChildSurface::attach(target) }?);
        let (Some(display), Some(window)) = (child.display(), child.surface()) else {
            return Err(ClientError::WindowHandleInvalid);
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(wgpu::rwh::RawDisplayHandle::Wayland(
                    wgpu::rwh::WaylandDisplayHandle::new(display),
                )),
                raw_window_handle: wgpu::rwh::RawWindowHandle::Wayland(
                    wgpu::rwh::WaylandWindowHandle::new(window),
                ),
            })
        }
        .map_err(ClientError::WindowSurface)?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..wgpu::RequestAdapterOptions::default()
        }))
        .map_err(ClientError::WindowAdapter)?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&rusplay_scale::device_descriptor(&adapter)))
                .map_err(ClientError::WindowDevice)?;
        let gpu = Gpu { device, queue };

        let display = display_for(config)?;
        let options = SurfaceOptions::choose(&surface, &adapter, config, &display)?;
        let shared = Arc::new(Shared::new(options.output.encoding.target));
        let presenter = Presenter {
            composer: Composer::new(&gpu.device, &gpu.queue, &options.output.encoding),
            cursor_layer: CursorLayer::new(&gpu.device, config.scaling),
            gpu: gpu.clone(),
            surface,
            adapter,
            child: Arc::clone(&child),
            choice: OutputChoice::new(display, options.output),
            overrides: config.color.target,
            output_config: config.color.output,
            options,
            configured: None,
            destination: None,
            frame: None,
            cursor: None,
        };

        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(format!("rusplay-client-{}", Stage::Window))
            .spawn(move || presenter.run(&worker))
            .map_err(|source| ClientError::SpawnStage {
                stage: Stage::Window,
                source,
            })?;

        let follower = config
            .color
            .output
            .query_display
            .then(|| {
                let notify = Arc::clone(&shared);
                DisplayFollower::start(Arc::clone(&child), move |image| {
                    notify.update(|pending| pending.display = Some(image));
                })
                .ok()
            })
            .flatten();

        Ok(Self {
            target,
            capture: None,
            gpu,
            shared,
            surface: child,
            presenter: Some(thread),
            layout: None,
            follower,
        })
    }

    pub fn color_target(&self) -> ColorVolume {
        *self.shared.color_target.lock()
    }

    pub fn gpu(&self) -> Gpu {
        self.gpu.clone()
    }

    pub fn sink(&self) -> WindowSink {
        WindowSink {
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn follow_cursor(&self, cursor: &LatestCursor) {
        let watcher: Arc<dyn CursorWatcher> = Arc::clone(&self.shared) as Arc<dyn CursorWatcher>;
        cursor.watch(watcher);
        let cursor = cursor.clone();

        self.shared.update(|pending| pending.cursor = Some(cursor));
    }

    pub fn capture_input(
        &mut self,
        stream: &ClientStream,
        input: &InputConfig,
    ) -> Result<(), ClientError> {
        if self.capture.is_some() {
            return Ok(());
        }

        let handle = stream.input();
        let capture = unsafe {
            WaylandCapture::attach(self.target, CaptureOptions::from(input), move |event| {
                handle.send(event);
            })
        }?;
        capture.set_region(self.layout.map(WindowLayout::region));

        let follower = Arc::new(PointerFollower {
            cursor: stream.cursor(),
            observer: capture.host_cursor_observer(),
            last_visible: Mutex::new(None),
        });
        stream
            .cursor()
            .watch(Arc::clone(&follower) as Arc<dyn CursorWatcher>);
        follower.changed();

        self.capture = Some(capture);

        Ok(())
    }

    pub fn is_engaged(&self) -> bool {
        self.capture
            .as_ref()
            .is_some_and(WaylandCapture::is_engaged)
    }

    pub fn pointer_mode(&self) -> Option<PointerMode> {
        self.capture.as_ref().map(WaylandCapture::pointer_mode)
    }

    pub fn cycle_pointer_mode(&self) {
        if let Some(capture) = &self.capture {
            capture.cycle_pointer_mode();
        }
    }

    pub fn active_pointer(&self) -> Option<ActivePointer> {
        self.capture.as_ref().map(WaylandCapture::active_pointer)
    }

    pub fn set_pointer_mode(&self, mode: PointerMode) {
        if let Some(capture) = &self.capture {
            capture.set_pointer_mode(mode);
        }
    }

    pub fn take_actions(&self) -> Vec<ClientAction> {
        self.capture
            .as_ref()
            .map_or_else(Vec::new, WaylandCapture::take_actions)
    }

    pub fn release(&self) {
        if let Some(capture) = &self.capture {
            capture.release();
        }
    }

    pub fn layout(&self) -> Option<WindowLayout> {
        self.layout
    }

    pub fn set_layout(&mut self, layout: Option<WindowLayout>) -> Result<bool, ClientError> {
        if layout == self.layout {
            return Ok(false);
        }

        if let Some(layout) = layout {
            self.surface.set_position(layout.x, layout.y)?;
        }
        if let Some(capture) = &self.capture {
            capture.set_region(layout.map(WindowLayout::region));
        }
        self.layout = layout;
        self.shared.update(|pending| pending.layout = layout);

        Ok(true)
    }

    pub fn is_finished(&self) -> bool {
        self.presenter.as_ref().is_none_or(JoinHandle::is_finished)
            || self
                .capture
                .as_ref()
                .is_some_and(WaylandCapture::is_finished)
    }

    pub fn stop(mut self) -> Result<(), ClientError> {
        self.shut_down()
    }

    fn shut_down(&mut self) -> Result<(), ClientError> {
        let captured = self.capture.take().map_or(Ok(()), WaylandCapture::stop);
        let followed = self.follower.take().map_or(Ok(()), DisplayFollower::stop);

        self.shared.update(|pending| {
            pending.stop = true;
            pending.frame = None;
        });

        let presented = match self.presenter.take() {
            Some(thread) => thread.join().unwrap_or(Err(ClientError::StagePanicked {
                stage: Stage::Window,
            })),
            None => Ok(()),
        };

        captured?;
        presented?;
        followed
    }
}

impl Drop for StreamWindow {
    fn drop(&mut self) {
        self.shut_down().ok();
    }
}

fn display_for(config: &Config) -> Result<Display, ClientError> {
    let overrides = &config.color.target;
    let queried = config
        .color
        .output
        .query_display
        .then(|| query_display(None).ok())
        .flatten();

    match queried {
        Some(image) => Ok(Display::Managed(resolve_target(&image, overrides)?)),
        None => Ok(Display::Unmanaged(resolve_target(
            &DisplayImage::default(),
            overrides,
        )?)),
    }
}

#[derive(Debug, Clone, Copy)]
struct SurfaceOptions {
    output: SurfaceOutput,
    present_mode: wgpu::PresentMode,
    alpha_mode: wgpu::CompositeAlphaMode,
    frame_latency: u32,
}

impl SurfaceOptions {
    fn choose(
        surface: &wgpu::Surface<'_>,
        adapter: &wgpu::Adapter,
        config: &Config,
        display: &Display,
    ) -> Result<Self, ClientError> {
        let capabilities = surface.get_capabilities(adapter);

        let output = SurfaceOutput::choose(
            &capabilities.format_capabilities,
            display,
            config.color.output,
        )?;
        let present_mode = wgpu_present_mode(config.video.present_mode);
        if !capabilities.present_modes.contains(&present_mode) {
            return Err(ClientError::PresentModeUnsupported {
                mode: config.video.present_mode,
            });
        }
        let alpha_mode = if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::Opaque)
        {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            capabilities
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };

        Ok(Self {
            output,
            present_mode,
            alpha_mode,
            frame_latency: u32::from(config.video.present_frame_latency),
        })
    }

    fn configuration(&self, size: Resolution) -> wgpu::SurfaceConfiguration {
        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.output.format,
            color_space: self.output.color_space,
            width: size.width,
            height: size.height,
            present_mode: self.present_mode,
            desired_maximum_frame_latency: self.frame_latency,
            alpha_mode: self.alpha_mode,
            view_formats: Vec::new(),
        }
    }
}

fn wgpu_present_mode(mode: PresentMode) -> wgpu::PresentMode {
    match mode {
        PresentMode::Mailbox => wgpu::PresentMode::Mailbox,
        PresentMode::Immediate => wgpu::PresentMode::Immediate,
        PresentMode::Fifo => wgpu::PresentMode::Fifo,
    }
}

struct Presenter {
    gpu: Gpu,
    surface: wgpu::Surface<'static>,
    adapter: wgpu::Adapter,
    child: Arc<ChildSurface>,
    choice: OutputChoice,
    overrides: DisplayTargetConfig,
    output_config: DisplayOutputConfig,
    options: SurfaceOptions,
    composer: Composer,
    cursor_layer: CursorLayer,
    configured: Option<Resolution>,
    destination: Option<(u32, u32)>,
    frame: Option<PresentedFrame>,
    cursor: Option<LatestCursor>,
}

impl Presenter {
    fn run(mut self, shared: &Shared) -> Result<(), ClientError> {
        loop {
            let (layout, display) = {
                let mut pending = shared.pending.lock();
                while !pending.dirty && !pending.stop {
                    shared.wake.wait(&mut pending);
                }
                if pending.stop {
                    break;
                }

                pending.dirty = false;
                if let Some(frame) = pending.frame.take()
                    && self.choice.accepts(frame.color)
                {
                    self.frame = Some(frame);
                }
                if let Some(cursor) = pending.cursor.take() {
                    self.cursor = Some(cursor);
                }
                (pending.layout, pending.display.take())
            };

            if let Some(image) = display {
                self.follow(&image, shared);
            }
            self.draw(layout)?;
        }

        self.frame = None;
        Ok(())
    }

    fn follow(&mut self, image: &DisplayImage, shared: &Shared) {
        let target = self
            .choice
            .observe(image, &self.overrides, self.output_config, || {
                self.surface
                    .get_capabilities(&self.adapter)
                    .format_capabilities
            });

        if let Some(target) = target {
            *shared.color_target.lock() = target;
        }
    }

    fn draw(&mut self, layout: Option<WindowLayout>) -> Result<(), ClientError> {
        let (Some(layout), Some(frame)) = (layout, self.frame.as_ref()) else {
            return Ok(());
        };
        if frame.resolution() != layout.pixels {
            return Ok(());
        }

        match self.choice.admit(frame.color) {
            Admission::Reject => return Ok(()),
            Admission::Draw => {}
            Admission::Switch(output) => {
                self.options.output = output;
                self.composer.set_encoding(&output.encoding);
                self.configured = None;
            }
        }

        if self.configured != Some(layout.pixels) {
            self.surface
                .configure(&self.gpu.device, &self.options.configuration(layout.pixels));
            self.configured = Some(layout.pixels);
        }

        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => texture,
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                self.configured = None;
                texture
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.configured = None;
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => return Err(ClientError::SurfaceLost),
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(ClientError::SurfaceValidation);
            }
        };

        let format = self.options.output.format;
        let target = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let picture = frame.view();
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rusplay-window"),
            });

        let cursor_state = self.cursor.as_ref().and_then(LatestCursor::latest);
        let cursor = match cursor_state {
            Some(state) => self.cursor_layer.prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut encoder,
                &state,
                layout.pixels,
            )?,
            None => None,
        };

        self.composer.encode(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            &picture,
            cursor.as_ref().map(|(view, placement)| (view, *placement)),
            &target,
            format,
        );

        let destination = (layout.width, layout.height);
        if self.destination != Some(destination) {
            self.child.set_destination(layout.width, layout.height);
            self.destination = Some(destination);
        }

        self.gpu.queue.submit([encoder.finish()]);
        self.gpu.queue.present(texture);

        Ok(())
    }
}
