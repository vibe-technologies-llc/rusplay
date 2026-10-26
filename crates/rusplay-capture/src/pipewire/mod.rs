mod color;
mod convert;
mod cursor_meta;
mod format;
mod raw;

use std::{
    cell::RefCell,
    os::fd::OwnedFd,
    rc::Rc,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use pipewire::{
    channel,
    context::ContextRc,
    keys,
    main_loop::{MainLoopRc, MainLoopWeak},
    properties::properties,
    spa::{
        buffer::{ChunkFlags, DataType},
        param::ParamType,
        pod::Pod,
        sys as spa_sys,
        utils::Direction,
    },
    stream::{Stream, StreamFlags, StreamRc, StreamState},
};
use rusplay_config::{CaptureConfig, CursorMode, VideoConfig};
use rusplay_core::{BitDepth, ColorSpace, FrameRate, HdrLevels, Resolution, Timestamp};

pub use self::format::default_formats;
use self::{
    color::describe_color,
    convert::{DmaBufPlane, ShmPlane, dmabuf_frame, shm_frame},
    cursor_meta::{cursor_meta_param, parse_cursor_meta},
    format::{
        ColorOffer, FormatDecision, FormatRequest, ModifierRequest, NegotiatedFormat,
        buffers_param, decide, dmabuf_enum_format, enum_formats, header_meta_param, parse_format,
    },
    raw::{
        BufferHandle, DequeuedBuffer, HeldBuffers, borrow_fd, header_is_corrupted, library_version,
    },
};
use crate::{
    BufferLease, CaptureError, CaptureSource, DrmFourcc, Frame, FrameBuffer, PortalSession,
    ShmFallback,
    cursor::{CursorTracker, CursorUpdate},
    handoff::{Publisher, Received, Subscriber, latest},
    modifier::FormatModifiers,
};

const STREAM_NAME: &str = "rusplay-capture";
const THREAD_NAME: &str = "rusplay-pipewire";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOptions {
    pub formats: Vec<DrmFourcc>,
    pub prefer_dmabuf: bool,
    pub modifiers: Vec<FormatModifiers>,
    pub cursor_metadata: bool,
    pub color_preference: Option<ColorSpace>,
    pub hdr: HdrLevels,
    pub size_hint: Option<Resolution>,
    pub frame_rate: FrameRate,
}

impl StreamOptions {
    pub fn new(capture: &CaptureConfig, video: &VideoConfig) -> Self {
        let formats = default_formats(video.pixel_format.depth != BitDepth::Eight);
        let modifiers = formats
            .iter()
            .copied()
            .map(FormatModifiers::with_defaults)
            .collect();

        Self {
            formats,
            prefer_dmabuf: capture.prefer_dmabuf,
            modifiers,
            cursor_metadata: capture.cursor == CursorMode::Metadata,
            color_preference: Some(video.color),
            hdr: video.color.hdr,
            size_hint: video.resolution,
            frame_rate: video.frame_rate,
        }
    }

    #[must_use]
    pub fn with_modifiers(mut self, modifiers: Vec<FormatModifiers>) -> Self {
        self.modifiers = modifiers;
        self
    }

    fn request(&self) -> FormatRequest {
        FormatRequest {
            size_hint: self.size_hint,
            frame_rate: self.frame_rate,
            color: self.color_preference.as_ref().map(ColorOffer::preferring),
        }
    }

    fn dmabuf_table(&self) -> Option<&[FormatModifiers]> {
        self.prefer_dmabuf.then_some(self.modifiers.as_slice())
    }

    fn enum_formats(&self) -> Result<Vec<Vec<u8>>, CaptureError> {
        enum_formats(&self.formats, self.dmabuf_table(), self.request())
    }
}

enum Command {
    Release(BufferHandle),
    Stop,
}

type Failure = Arc<Mutex<Option<CaptureError>>>;

pub struct PipeWireCapture {
    deliveries: Subscriber<Frame>,
    cursor: Subscriber<CursorUpdate>,
    failure: Failure,
    fallback: ShmFallback,
    control: channel::Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl PipeWireCapture {
    pub fn connect(
        remote: OwnedFd,
        node_id: u32,
        options: StreamOptions,
    ) -> Result<Self, CaptureError> {
        let (publisher, deliveries) = latest();
        let (cursor_publisher, cursor) = latest();
        let failure = Failure::default();
        let fallback = ShmFallback::default();
        let (control, commands) = channel::channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);

        let setup = WorkerSetup {
            remote,
            node_id,
            options,
            publisher,
            cursor_publisher,
            failure: Arc::clone(&failure),
            fallback: fallback.clone(),
            control: control.clone(),
            commands,
        };
        let worker = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || {
                if let Err(error) = run_worker(setup, &started_tx) {
                    let _ = started_tx.send(Err(error));
                }
            })
            .map_err(CaptureError::ThreadSpawn)?;

        let mut capture = Self {
            deliveries,
            cursor,
            failure,
            fallback,
            control,
            worker: Some(worker),
        };
        match started_rx.recv() {
            Ok(Ok(())) => Ok(capture),
            Ok(Err(error)) => {
                capture.shutdown();
                Err(error)
            }
            Err(mpsc::RecvError) => {
                capture.shutdown();
                Err(CaptureError::ThreadExited)
            }
        }
    }

    pub fn from_portal(
        session: PortalSession,
        mut options: StreamOptions,
    ) -> Result<Self, CaptureError> {
        let PortalSession {
            remote, streams, ..
        } = session;
        let stream = streams.first().ok_or(CaptureError::NoStreams)?;

        if options.size_hint.is_none() {
            options.size_hint = stream.size;
        }

        Self::connect(remote, stream.node_id, options)
    }

    pub fn try_next_frame(&mut self) -> Result<Option<Frame>, CaptureError> {
        let received = self.deliveries.try_recv();
        self.resolve(received)
    }

    pub fn next_frame_timeout(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        let received = self.deliveries.recv_timeout(timeout);
        self.resolve(received)
    }

    pub fn superseded_frames(&self) -> u64 {
        self.deliveries.superseded()
    }

    pub fn shm_fallback(&self) -> ShmFallback {
        self.fallback.clone()
    }

    fn resolve(&self, received: Received<Frame>) -> Result<Option<Frame>, CaptureError> {
        match received {
            Received::Value(frame) => Ok(Some(frame)),
            Received::Empty => Ok(None),
            Received::Closed => Err(self.take_failure()),
        }
    }

    fn take_failure(&self) -> CaptureError {
        self.failure
            .lock()
            .take()
            .unwrap_or(CaptureError::StreamEnded)
    }

    fn shutdown(&mut self) {
        let _ = self.control.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl CaptureSource for PipeWireCapture {
    fn next_frame(&mut self) -> Result<Frame, CaptureError> {
        self.deliveries.recv().ok_or_else(|| self.take_failure())
    }

    fn cursor_feed(&self) -> Option<Subscriber<CursorUpdate>> {
        Some(self.cursor.clone())
    }
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct WorkerSetup {
    remote: OwnedFd,
    node_id: u32,
    options: StreamOptions,
    publisher: Publisher<Frame>,
    cursor_publisher: Publisher<CursorUpdate>,
    failure: Failure,
    fallback: ShmFallback,
    control: channel::Sender<Command>,
    commands: channel::Receiver<Command>,
}

struct Worker {
    options: StreamOptions,
    format: Option<NegotiatedFormat>,
    publisher: Publisher<Frame>,
    cursor_publisher: Publisher<CursorUpdate>,
    cursor: CursorTracker,
    held: Rc<RefCell<HeldBuffers>>,
    failure: Failure,
    fallback: ShmFallback,
    control: channel::Sender<Command>,
    mainloop: MainLoopWeak,
    epoch: Instant,
}

impl Worker {
    fn fail(&mut self, error: CaptureError) {
        self.failure.lock().get_or_insert(error);
        self.stop();
    }

    fn stop(&mut self) {
        self.publisher.close();
        self.cursor_publisher.close();
        if let Some(mainloop) = self.mainloop.upgrade() {
            mainloop.quit();
        }
    }

    fn timestamp(&self) -> Timestamp {
        Timestamp::from_micros(u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX))
    }

    fn on_state_changed(&mut self, new: StreamState) {
        match new {
            StreamState::Error(message) => self.fail(CaptureError::StreamFailed { message }),
            StreamState::Unconnected => self.stop(),
            StreamState::Connecting | StreamState::Paused | StreamState::Streaming => {}
        }
    }

    fn on_param_changed(&mut self, stream: &Stream, id: u32, param: Option<&Pod>) {
        if id != ParamType::Format.as_raw() {
            return;
        }
        self.format = None;
        let Some(param) = param else {
            return;
        };

        if let Err(error) = self.negotiate(stream, param) {
            self.fail(error);
        }
    }

    fn negotiate(&mut self, stream: &Stream, param: &Pod) -> Result<(), CaptureError> {
        let parsed = parse_format(param.as_bytes())?;
        let table = self.options.dmabuf_table().unwrap_or_default();

        let color = describe_color(&parsed.colorimetry, self.options.hdr)?;

        let params = match decide(&parsed, table, color) {
            FormatDecision::Accept(format) => {
                self.format = Some(format);
                let mut params = vec![buffers_param(format.memory())?, header_meta_param()?];
                if self.options.cursor_metadata {
                    params.push(cursor_meta_param()?);
                }
                params
            }
            FormatDecision::Fixate { fourcc, modifier } => {
                let fixated = dmabuf_enum_format(
                    fourcc,
                    ModifierRequest::Fixed(modifier),
                    self.options.request(),
                )?;
                fixated
                    .into_iter()
                    .chain(self.options.enum_formats()?)
                    .collect()
            }
            FormatDecision::FallBackToShm => {
                enum_formats(&self.options.formats, None, self.options.request())?
            }
        };

        stream.update_params(&mut as_pods(&params)?)?;
        Ok(())
    }

    fn fall_back_if_requested(&mut self, stream: &Stream) -> Result<(), CaptureError> {
        if !self.options.prefer_dmabuf || !self.fallback.is_requested() {
            return Ok(());
        }

        self.options.prefer_dmabuf = false;
        let params = self.options.enum_formats()?;
        stream.update_params(&mut as_pods(&params)?)?;
        Ok(())
    }

    fn lease(&self, buffer: DequeuedBuffer<'_>) -> BufferLease {
        let handle = self.held.borrow_mut().hold(buffer);
        let control = self.control.clone();

        BufferLease::new(move || {
            let _ = control.send(Command::Release(handle));
        })
    }

    fn on_process(&mut self, stream: &Stream) {
        if let Err(error) = self.fall_back_if_requested(stream) {
            self.fail(error);
            return;
        }

        let mut newest = None;
        while let Some(buffer) = DequeuedBuffer::dequeue(stream) {
            self.observe_cursor(&buffer);
            newest = Some(buffer);
        }
        let Some(mut buffer) = newest else {
            return;
        };

        let mut frame = match self.convert(&mut buffer) {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(error) => {
                drop(buffer);
                self.fail(error);
                return;
            }
        };

        if let FrameBuffer::DmaBuf { lease, .. } = &mut frame.buffer {
            *lease = self.lease(buffer);
        }
        drop(self.publisher.replace(frame));
    }

    fn observe_cursor(&mut self, buffer: &DequeuedBuffer<'_>) {
        let Some(observation) = buffer
            .meta(spa_sys::SPA_META_Cursor)
            .and_then(parse_cursor_meta)
        else {
            return;
        };

        if let Some(update) = self.cursor.observe(observation) {
            self.cursor_publisher.merge(update, CursorUpdate::absorb);
        }
    }

    fn convert(&self, buffer: &mut DequeuedBuffer<'_>) -> Result<Option<Frame>, CaptureError> {
        if buffer
            .meta(spa_sys::SPA_META_Header)
            .is_some_and(header_is_corrupted)
        {
            return Ok(None);
        }

        let format = self.format.ok_or(CaptureError::FormatNotNegotiated)?;
        let timestamp = self.timestamp();
        let datas = buffer.datas_mut();
        let Some(first) = datas.first() else {
            return Ok(None);
        };

        let chunk = first.chunk();
        if chunk.size() == 0 || chunk.flags().contains(ChunkFlags::CORRUPTED) {
            return Ok(None);
        }

        let data_type = first.type_();
        if data_type == DataType::DmaBuf {
            let planes = datas
                .iter()
                .map(|data| {
                    Some(DmaBufPlane {
                        fd: borrow_fd(data)?,
                        offset: data.chunk().offset(),
                        stride: data.chunk().stride(),
                    })
                })
                .collect::<Option<Vec<_>>>()
                .ok_or(CaptureError::NoPlanes)?;
            return dmabuf_frame(format, &planes, timestamp).map(Some);
        }
        if data_type != DataType::MemPtr && data_type != DataType::MemFd {
            return Err(CaptureError::UnsupportedBufferData {
                data_type: data_type.as_raw(),
            });
        }

        let offset = chunk.offset();
        let stride = chunk.stride();
        let Some(mapped) = datas.first_mut().and_then(|data| data.data()) else {
            return Ok(None);
        };
        shm_frame(
            format,
            ShmPlane {
                mapped,
                offset,
                stride,
            },
            timestamp,
        )
        .map(Some)
    }
}

const DROP_FLAG_VERSION: (u32, u32, u32) = (1, 5, 81);

fn drops_unmatched_properties(version: &str) -> bool {
    let mut parts = version
        .split('.')
        .map(|part| part.trim().parse::<u32>().unwrap_or(0));
    let parsed = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );

    parsed >= DROP_FLAG_VERSION
}

fn as_pods(params: &[Vec<u8>]) -> Result<Vec<&Pod>, CaptureError> {
    params
        .iter()
        .map(|bytes| Pod::from_bytes(bytes).ok_or(CaptureError::FormatPodMalformed))
        .collect()
}

fn run_worker(
    setup: WorkerSetup,
    started: &mpsc::SyncSender<Result<(), CaptureError>>,
) -> Result<(), CaptureError> {
    pipewire::init();

    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(setup.remote, None)?;
    let stream = StreamRc::new(
        core,
        STREAM_NAME,
        properties! {
            *keys::MEDIA_TYPE => "Video",
            *keys::MEDIA_CATEGORY => "Capture",
            *keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let mut options = setup.options;
    if !drops_unmatched_properties(&library_version()) {
        options.color_preference = None;
    }
    let params = options.enum_formats()?;
    let held = Rc::new(RefCell::new(HeldBuffers::default()));
    let worker = Worker {
        options,
        format: None,
        publisher: setup.publisher,
        cursor_publisher: setup.cursor_publisher,
        cursor: CursorTracker::default(),
        held: Rc::clone(&held),
        failure: setup.failure,
        fallback: setup.fallback,
        control: setup.control,
        mainloop: mainloop.downgrade(),
        epoch: Instant::now(),
    };
    let _listener = stream
        .add_local_listener_with_user_data(worker)
        .state_changed(|_, worker, _, new| worker.on_state_changed(new))
        .param_changed(|stream, worker, id, param| worker.on_param_changed(stream, id, param))
        .process(|stream, worker| worker.on_process(stream))
        .remove_buffer(|_, worker, buffer| worker.held.borrow_mut().forget(buffer))
        .register()?;

    stream.connect(
        Direction::Input,
        Some(setup.node_id),
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut as_pods(&params)?,
    )?;

    let _commands = setup.commands.attach(mainloop.loop_(), {
        let mainloop = mainloop.downgrade();
        let stream = stream.clone();
        move |command| match command {
            Command::Release(handle) => held.borrow_mut().release(handle, &stream),
            Command::Stop => {
                if let Some(mainloop) = mainloop.upgrade() {
                    mainloop.quit();
                }
            }
        }
    });

    let _ = started.send(Ok(()));
    mainloop.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_is_offered_only_where_unmatched_properties_are_dropped() {
        assert!(drops_unmatched_properties("1.6.10"));
        assert!(drops_unmatched_properties("1.5.81"));
        assert!(!drops_unmatched_properties("1.5.80"));
        assert!(!drops_unmatched_properties("1.4.9"));
        assert!(!drops_unmatched_properties("0.3.65"));
        assert!(!drops_unmatched_properties(""));
    }

    #[test]
    fn stream_options_prefer_the_stream_colour() {
        let mut video = VideoConfig::default();
        video.color.transfer = rusplay_core::TransferFunction::Pq;

        let options = StreamOptions::new(&CaptureConfig::default(), &video);

        assert_eq!(options.color_preference, Some(video.color));
        assert_eq!(
            options.request().color,
            Some(ColorOffer::preferring(&video.color))
        );
    }
}
