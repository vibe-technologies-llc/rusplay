mod convert;
mod format;

use std::{
    os::fd::OwnedFd,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use pipewire::{
    buffer::Buffer,
    channel,
    context::ContextRc,
    keys,
    main_loop::{MainLoopRc, MainLoopWeak},
    properties::properties,
    spa::{
        buffer::{
            ChunkFlags, DataType,
            meta::{MetaHeader, MetaHeaderFlags},
        },
        param::ParamType,
        pod::Pod,
        utils::Direction,
    },
    stream::{Stream, StreamFlags, StreamRc, StreamState},
};
use rusplay_config::VideoConfig;
use rusplay_core::{BitDepth, FrameRate, Resolution, Timestamp};

pub use self::format::default_formats;
use self::{
    convert::{ShmPlane, shm_frame},
    format::{
        BufferMemory, FormatRequest, NegotiatedFormat, buffers_param, header_meta_param,
        parse_format, shm_enum_format,
    },
};
use crate::{
    CaptureError, CaptureSource, DrmFourcc, Frame, PortalSession,
    handoff::{Publisher, Received, Subscriber, latest},
};

const STREAM_NAME: &str = "rusplay-capture";
const THREAD_NAME: &str = "rusplay-pipewire";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOptions {
    pub formats: Vec<DrmFourcc>,
    pub size_hint: Option<Resolution>,
    pub frame_rate: FrameRate,
}

impl StreamOptions {
    pub fn new(video: &VideoConfig) -> Self {
        Self {
            formats: default_formats(video.pixel_format.depth != BitDepth::Eight),
            size_hint: video.resolution,
            frame_rate: video.frame_rate,
        }
    }

    fn request(&self) -> FormatRequest {
        FormatRequest {
            size_hint: self.size_hint,
            frame_rate: self.frame_rate,
        }
    }
}

enum Command {
    Stop,
}

type Failure = Arc<Mutex<Option<CaptureError>>>;

pub struct PipeWireCapture {
    frames: Subscriber<Frame>,
    failure: Failure,
    control: channel::Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl PipeWireCapture {
    pub fn connect(
        remote: OwnedFd,
        node_id: u32,
        options: StreamOptions,
    ) -> Result<Self, CaptureError> {
        let (publisher, frames) = latest();
        let failure = Failure::default();
        let (control, commands) = channel::channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);

        let setup = WorkerSetup {
            remote,
            node_id,
            options,
            publisher,
            failure: Arc::clone(&failure),
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
            frames,
            failure,
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
        self.resolve(self.frames.try_recv())
    }

    pub fn next_frame_timeout(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        self.resolve(self.frames.recv_timeout(timeout))
    }

    pub fn superseded_frames(&self) -> u64 {
        self.frames.superseded()
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
        self.frames.recv().ok_or_else(|| self.take_failure())
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
    failure: Failure,
    commands: channel::Receiver<Command>,
}

struct Worker {
    format: Option<NegotiatedFormat>,
    publisher: Publisher<Frame>,
    failure: Failure,
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
        let Some(param) = param else {
            self.format = None;
            return;
        };

        if let Err(error) = self.accept_format(stream, param) {
            self.fail(error);
        }
    }

    fn accept_format(&mut self, stream: &Stream, param: &Pod) -> Result<(), CaptureError> {
        let format = parse_format(param.as_bytes())?;
        self.format = Some(format);

        let params = [buffers_param(BufferMemory::Shared)?, header_meta_param()?];
        stream.update_params(&mut as_pods(&params)?)?;
        Ok(())
    }

    fn on_process(&mut self, stream: &Stream) {
        let Some(mut buffer) = latest_buffer(stream) else {
            return;
        };

        match self.convert(&mut buffer) {
            Ok(Some(frame)) => {
                self.publisher.replace(frame);
            }
            Ok(None) => {}
            Err(error) => self.fail(error),
        }
    }

    fn convert(&self, buffer: &mut Buffer<'_>) -> Result<Option<Frame>, CaptureError> {
        if buffer
            .find_meta::<MetaHeader>()
            .is_some_and(|header| header.flags().contains(MetaHeaderFlags::CORRUPTED))
        {
            return Ok(None);
        }

        let format = self.format.ok_or(CaptureError::FormatNotNegotiated)?;
        let timestamp = self.timestamp();
        let Some(data) = buffer.datas_mut().first_mut() else {
            return Ok(None);
        };

        let chunk = data.chunk();
        if chunk.size() == 0 || chunk.flags().contains(ChunkFlags::CORRUPTED) {
            return Ok(None);
        }
        let offset = chunk.offset();
        let stride = chunk.stride();

        let data_type = data.type_();
        if data_type != DataType::MemPtr && data_type != DataType::MemFd {
            return Err(CaptureError::UnsupportedBufferData {
                data_type: data_type.as_raw(),
            });
        }
        let Some(mapped) = data.data() else {
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

fn latest_buffer(stream: &Stream) -> Option<Buffer<'_>> {
    let mut newest = stream.dequeue_buffer()?;
    while let Some(newer) = stream.dequeue_buffer() {
        newest = newer;
    }
    Some(newest)
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

    let worker = Worker {
        format: None,
        publisher: setup.publisher,
        failure: setup.failure,
        mainloop: mainloop.downgrade(),
        epoch: Instant::now(),
    };
    let _listener = stream
        .add_local_listener_with_user_data(worker)
        .state_changed(|_, worker, _, new| worker.on_state_changed(new))
        .param_changed(|stream, worker, id, param| worker.on_param_changed(stream, id, param))
        .process(|stream, worker| worker.on_process(stream))
        .register()?;

    let params: Vec<Vec<u8>> = shm_enum_format(&setup.options.formats, setup.options.request())?
        .into_iter()
        .collect();
    stream.connect(
        Direction::Input,
        Some(setup.node_id),
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut as_pods(&params)?,
    )?;

    let _commands = setup.commands.attach(mainloop.loop_(), {
        let mainloop = mainloop.downgrade();
        move |command| match command {
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
