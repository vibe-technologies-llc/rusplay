mod compose;
mod cursor;
mod layout;
mod surface;

use std::{
    sync::Arc,
    thread::{self, JoinHandle},
};

use parking_lot::{Condvar, Mutex};
use rusplay_config::{Config, InputConfig, PresentMode};
use rusplay_core::Resolution;
pub use rusplay_input::wayland::WaylandTarget;
use rusplay_input::{capture::CaptureOptions, wayland::WaylandCapture};
use rusplay_scale::OutputLight;

pub use crate::window::layout::{LogicalArea, WindowLayout};
use crate::{
    ClientError, ClientStream, FrameSink, Gpu, LatestCursor, PresentedFrame, Stage,
    cursor::CursorWatcher,
    window::{compose::Composer, cursor::CursorLayer, surface::ChildSurface},
};

const SURFACE_FORMATS: [wgpu::TextureFormat; 2] = [
    wgpu::TextureFormat::Bgra8Unorm,
    wgpu::TextureFormat::Rgba8Unorm,
];

#[derive(Debug, Default)]
struct Pending {
    frame: Option<PresentedFrame>,
    layout: Option<WindowLayout>,
    cursor: Option<LatestCursor>,
    dirty: bool,
    stop: bool,
}

#[derive(Debug, Default)]
struct Shared {
    pending: Mutex<Pending>,
    wake: Condvar,
}

impl Shared {
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

#[derive(Debug, Clone)]
pub struct WindowSink {
    shared: Arc<Shared>,
}

impl FrameSink for WindowSink {
    fn present(&mut self, frame: PresentedFrame) {
        let replaced = self.shared.update(|pending| pending.frame.replace(frame));

        drop(replaced);
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

        let options = SurfaceOptions::choose(&surface, &adapter, config)?;
        let presenter = Presenter {
            composer: Composer::new(&gpu.device),
            cursor_layer: CursorLayer::new(&gpu.device, config.scaling),
            gpu: gpu.clone(),
            surface,
            child: Arc::clone(&child),
            options,
            configured: None,
            destination: None,
            frame: None,
            cursor: None,
        };

        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(format!("rusplay-client-{}", Stage::Window))
            .spawn(move || presenter.run(&worker))
            .map_err(|source| ClientError::SpawnStage {
                stage: Stage::Window,
                source,
            })?;

        Ok(Self {
            target,
            capture: None,
            gpu,
            shared,
            surface: child,
            presenter: Some(thread),
            layout: None,
        })
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
        self.capture = Some(capture);

        Ok(())
    }

    pub fn is_engaged(&self) -> bool {
        self.capture
            .as_ref()
            .is_some_and(WaylandCapture::is_engaged)
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
        presented
    }
}

impl Drop for StreamWindow {
    fn drop(&mut self) {
        self.shut_down().ok();
    }
}

#[derive(Debug, Clone, Copy)]
struct SurfaceOptions {
    format: wgpu::TextureFormat,
    present_mode: wgpu::PresentMode,
    alpha_mode: wgpu::CompositeAlphaMode,
    frame_latency: u32,
}

impl SurfaceOptions {
    fn choose(
        surface: &wgpu::Surface<'_>,
        adapter: &wgpu::Adapter,
        config: &Config,
    ) -> Result<Self, ClientError> {
        let capabilities = surface.get_capabilities(adapter);

        let format = SURFACE_FORMATS
            .into_iter()
            .find(|format| capabilities.formats.contains(format))
            .ok_or(ClientError::SurfaceFormatUnsupported)?;
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
            format,
            present_mode,
            alpha_mode,
            frame_latency: u32::from(config.video.present_frame_latency),
        })
    }

    fn configuration(&self, size: Resolution) -> wgpu::SurfaceConfiguration {
        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width,
            height: size.height,
            present_mode: self.present_mode,
            desired_maximum_frame_latency: self.frame_latency,
            alpha_mode: self.alpha_mode,
            view_formats: vec![self.format.add_srgb_suffix()],
        }
    }

    fn view_format(&self, light: OutputLight) -> wgpu::TextureFormat {
        match light {
            OutputLight::Linear => self.format.add_srgb_suffix(),
            OutputLight::Encoded => self.format,
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
    child: Arc<ChildSurface>,
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
            let layout = {
                let mut pending = shared.pending.lock();
                while !pending.dirty && !pending.stop {
                    shared.wake.wait(&mut pending);
                }
                if pending.stop {
                    break;
                }

                pending.dirty = false;
                if let Some(frame) = pending.frame.take() {
                    self.frame = Some(frame);
                }
                if let Some(cursor) = pending.cursor.take() {
                    self.cursor = Some(cursor);
                }
                pending.layout
            };

            self.draw(layout)?;
        }

        self.frame = None;
        Ok(())
    }

    fn draw(&mut self, layout: Option<WindowLayout>) -> Result<(), ClientError> {
        let (Some(layout), Some(frame)) = (layout, self.frame.as_ref()) else {
            return Ok(());
        };
        if frame.resolution() != layout.pixels {
            return Ok(());
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

        let format = self.options.view_format(frame.color.light);
        let target = texture.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(format),
            ..wgpu::TextureViewDescriptor::default()
        });
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
                frame.source,
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
