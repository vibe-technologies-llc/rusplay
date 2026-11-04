use std::{
    fmt,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use parking_lot::RwLock;
use rusplay_audio::CODEC_THREAD_STACK_BYTES;
use rusplay_capture::OutputRequest;
use rusplay_clipboard::{ClipboardLink, ClipboardPolicy, ClipboardState};
use rusplay_config::{Config, SourceKind};
use rusplay_net::{
    ControlChannel, ControlConfig, FecScheme, PeerStore, Pin, Transport, TransportOptions,
};
use rusplay_priority::{ThreadRole, ThreadTuning};

use crate::{
    CaptureFeed, Client, Gpu, HostError, HostStats, Stage,
    accept::accept,
    audio::{self, AudioLinks, AudioSourceFactory, default_audio_source},
    capture,
    clipboard::{ClipboardOpener, default_clipboard_opener, open_link},
    control::{ControlLinks, ControlStage, HintPolicy},
    cursor::{self, CursorLinks},
    encode::{self, EncodeLinks, PipelineFactory},
    input::{self, InputFeed, InputLinks},
    microphone::{self, MicrophoneLinks, MicrophoneOutputFactory, default_microphone_output},
    negotiate::{NegotiatedFormat, configured_format},
    outbox::ControlOutbox,
    pipeline::GpuPipelineFactory,
    send::{self, Pacing, SendLinks, SendOptions},
    shared::{Shared, StopOnDrop},
    virtual_output::VirtualOutput,
};

const FEC_BLOCK_DATA_SHARDS: u8 = 32;
const SEND_QUEUE_FRAMES: usize = 1;
const INPUT_QUEUE_DATAGRAMS: usize = 256;

pub struct HostSession {
    config: Config,
    transport: Arc<Transport>,
    peers: Arc<RwLock<PeerStore>>,
    audio: Option<AudioSourceFactory>,
    microphone: Option<MicrophoneOutputFactory>,
    input: Option<Arc<InputFeed>>,
    clipboard: Option<ClipboardOpener>,
    busy: Arc<AtomicBool>,
}

impl fmt::Debug for HostSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostSession")
            .field("config", &self.config)
            .field("transport", &self.transport)
            .field("peers", &self.peers)
            .field("audio", &self.audio.is_some())
            .field("microphone", &self.microphone.is_some())
            .field("input", &self.input.is_some())
            .field("clipboard", &self.clipboard.is_some())
            .field("busy", &self.busy)
            .finish()
    }
}

impl HostSession {
    pub fn bind(config: Config, peers: PeerStore) -> Result<Self, HostError> {
        Self::bind_shared(config, Arc::new(RwLock::new(peers)))
    }

    pub fn bind_shared(config: Config, peers: Arc<RwLock<PeerStore>>) -> Result<Self, HostError> {
        let network = &config.network;
        let transport = Transport::bind(
            SocketAddr::new(network.bind_address, network.port),
            TransportOptions::from(network),
        )?;

        Ok(Self {
            config,
            transport: Arc::new(transport),
            peers,
            audio: default_audio_source(),
            microphone: default_microphone_output(),
            input: None,
            clipboard: default_clipboard_opener(),
            busy: Arc::new(AtomicBool::new(false)),
        })
    }

    #[must_use]
    pub fn with_audio_source(mut self, factory: AudioSourceFactory) -> Self {
        self.audio = Some(factory);
        self
    }

    #[must_use]
    pub fn without_audio(mut self) -> Self {
        self.audio = None;
        self
    }

    #[must_use]
    pub fn with_microphone_output(mut self, factory: MicrophoneOutputFactory) -> Self {
        self.microphone = Some(factory);
        self
    }

    #[must_use]
    pub fn without_microphone(mut self) -> Self {
        self.microphone = None;
        self
    }

    #[must_use]
    pub fn with_input(mut self, feed: Arc<InputFeed>) -> Self {
        self.input = Some(feed);
        self
    }

    #[must_use]
    pub fn with_clipboard(mut self, opener: ClipboardOpener) -> Self {
        self.clipboard = Some(opener);
        self
    }

    #[must_use]
    pub fn without_clipboard(mut self) -> Self {
        self.clipboard = None;
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn local_addr(&self) -> Result<SocketAddr, HostError> {
        Ok(self.transport.local_addr()?)
    }

    pub fn busy_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.busy)
    }

    pub fn peers(&self) -> &Arc<RwLock<PeerStore>> {
        &self.peers
    }

    pub fn accept(&self, pin: Option<Pin>) -> Result<Client, HostError> {
        accept(
            &self.transport,
            &self.peers,
            pin,
            &self.config.network,
            None,
        )
    }

    pub fn accept_cancellable(
        &self,
        pin: Option<Pin>,
        cancel: &AtomicBool,
    ) -> Result<Client, HostError> {
        accept(
            &self.transport,
            &self.peers,
            pin,
            &self.config.network,
            Some(cancel),
        )
    }

    pub fn serve(
        &self,
        client: &Client,
        gpu: Gpu,
        feed: &Arc<CaptureFeed>,
    ) -> Result<HostStream, HostError> {
        let factory = GpuPipelineFactory::new(gpu, self.config.clone(), feed.shm_fallback());

        self.serve_with(client, feed, Box::new(factory))
    }

    fn virtual_output(&self, feed: &CaptureFeed) -> Option<VirtualOutput> {
        (self.config.capture.source == SourceKind::Virtual).then(|| VirtualOutput {
            limits: self.config.capture.clone(),
            offer: feed.output_offer(),
        })
    }

    fn microphone_factory(&self) -> Result<Option<MicrophoneOutputFactory>, HostError> {
        if !self.config.audio.microphone.enabled {
            return Ok(None);
        }

        self.microphone
            .clone()
            .map(Some)
            .ok_or(HostError::MicrophoneNotCompiled)
    }

    fn open_clipboard(&self, stream: &mut HostStream) -> Option<ClipboardLink> {
        let (link, state) = open_link(
            self.clipboard.as_ref(),
            ClipboardPolicy::host(&self.config.input),
        );
        stream.clipboard = state;

        link
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn serve_with(
        &self,
        client: &Client,
        feed: &Arc<CaptureFeed>,
        factory: Box<dyn PipelineFactory>,
    ) -> Result<HostStream, HostError> {
        let network = &self.config.network;
        let shared = Arc::new(Shared::new(client.address()));
        let mut stream = HostStream {
            stages: Vec::new(),
            shared: Arc::clone(&shared),
            tuning: ThreadTuning::from_config(network),
            clipboard: ClipboardState::Off,
            _busy: BusyGuard::raise(&self.busy),
        };

        let outbox = Arc::new(ControlOutbox::new(
            ControlChannel::new(ControlConfig::from(network)),
            Arc::clone(&self.transport),
            Arc::clone(&shared.client),
        ));
        let (input_sender, input_datagrams) = mpsc::sync_channel(INPUT_QUEUE_DATAGRAMS);
        let hints = HintPolicy::from_config(&self.config);
        if hints != HintPolicy::Ignore {
            shared
                .counters
                .set_bitrate_target(self.config.video.bitrate_kbps);
        }
        let microphone = self.microphone_factory()?;
        let (microphone_sender, microphone_datagrams) =
            mpsc::sync_channel(usize::from(self.config.audio.decode_queue_packets));
        let control = ControlStage::new(
            network,
            hints,
            ControlLinks {
                shared: Arc::clone(&shared),
                transport: Arc::clone(&self.transport),
                peers: Arc::clone(&self.peers),
                peer: client.peer(),
                replay: client.replay().clone(),
                outbox: Arc::clone(&outbox),
                input: self.input.is_some().then_some(input_sender),
                microphone: microphone.is_some().then_some(microphone_sender),
                configured: configured_format(&self.config.video),
                virtual_output: self.virtual_output(feed),
                clipboard: self.open_clipboard(&mut stream),
            },
        )?;
        stream.spawn(Stage::Control, move || control.run())?;

        if let Some(feed) = &self.input {
            let links = InputLinks {
                shared: Arc::clone(&shared),
                feed: Arc::clone(feed),
                datagrams: input_datagrams,
                outbox: Arc::clone(&outbox),
            };
            stream.spawn(Stage::Input, move || input::run(&links))?;
        }

        let (output, queue) = mpsc::sync_channel(SEND_QUEUE_FRAMES);
        let send_links = SendLinks {
            shared: Arc::clone(&shared),
            queue,
            transport: Arc::clone(&self.transport),
        };
        let options = send_options(&self.config);
        stream.spawn(Stage::Send, move || send::run(options, &send_links))?;

        let encode_links = EncodeLinks {
            shared: Arc::clone(&shared),
            feed: Arc::clone(feed),
            output,
            outbox,
        };
        stream.spawn(Stage::Encode, move || encode::run(factory, &encode_links))?;

        if let Some(factory) = self.audio.clone() {
            let links = AudioLinks {
                shared: Arc::clone(&shared),
                transport: Arc::clone(&self.transport),
            };
            let audio = self.config.audio;
            stream.spawn_on(Stage::Audio, Some(CODEC_THREAD_STACK_BYTES), move || {
                audio::run(audio, &factory, &links)
            })?;
        }

        if let Some(factory) = microphone {
            let links = MicrophoneLinks {
                shared: Arc::clone(&shared),
                datagrams: microphone_datagrams,
            };
            let audio = self.config.audio;
            let network = self.config.network;
            stream.spawn_on(
                Stage::Microphone,
                Some(CODEC_THREAD_STACK_BYTES),
                move || microphone::run(audio, &network, &factory, &links),
            )?;
        }

        if let Some(subscriber) = feed.cursor_subscriber() {
            let links = CursorLinks {
                shared: Arc::clone(&shared),
                feed: Arc::clone(feed),
                subscriber,
                transport: Arc::clone(&self.transport),
            };
            let repeat = image_repeat(&self.config);
            stream.spawn(Stage::Cursor, move || cursor::run(repeat, &links))?;
        }

        let feed = Arc::clone(feed);
        stream.spawn(Stage::Capture, move || capture::run(&feed, &shared))?;

        Ok(stream)
    }
}

fn image_repeat(config: &Config) -> Option<Duration> {
    let millis = config.network.cursor_image_repeat_ms;

    (millis > 0).then(|| Duration::from_millis(u64::from(millis)))
}

fn send_options(config: &Config) -> SendOptions {
    let network = &config.network;
    let pacing = network
        .pacing
        .then(|| config.video.frame_rate.frame_interval())
        .flatten()
        .map(|frame_interval| Pacing {
            frame_interval,
            window_percent: network.pacing_window_percent,
            max_kbps: network.pacing_max_kbps(),
        });

    SendOptions {
        fec: FecScheme::for_overhead(FEC_BLOCK_DATA_SHARDS, network.fec_overhead_percent),
        pacing,
    }
}

const fn thread_role(stage: Stage) -> Option<ThreadRole> {
    match stage {
        Stage::Encode | Stage::Control => Some(ThreadRole::Frame),
        Stage::Send => Some(ThreadRole::TimerDriven),
        _ => None,
    }
}

#[derive(Debug)]
pub struct HostStream {
    stages: Vec<(Stage, JoinHandle<Result<(), HostError>>)>,
    shared: Arc<Shared>,
    tuning: ThreadTuning,
    clipboard: ClipboardState,
    _busy: BusyGuard,
}

#[derive(Debug)]
struct BusyGuard(Arc<AtomicBool>);

impl BusyGuard {
    fn raise(flag: &Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::Relaxed);
        Self(Arc::clone(flag))
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

impl HostStream {
    pub fn client(&self) -> SocketAddr {
        self.shared.client.address()
    }

    pub fn stats(&self) -> HostStats {
        self.shared.counters.snapshot()
    }

    pub fn format(&self) -> Option<NegotiatedFormat> {
        self.shared.negotiation.agreed()
    }

    pub fn virtual_output(&self) -> Option<OutputRequest> {
        self.shared.output()
    }

    pub fn clipboard(&self) -> &ClipboardState {
        &self.clipboard
    }

    pub fn is_finished(&self) -> bool {
        self.stages.iter().any(|(_, handle)| handle.is_finished())
    }

    pub fn stop(mut self) -> Result<(), HostError> {
        self.shared.stop();
        self.join()
    }

    pub fn wait(mut self) -> Result<(), HostError> {
        self.join()
    }

    fn join(&mut self) -> Result<(), HostError> {
        let mut first_error = None;

        for (stage, handle) in self.stages.drain(..) {
            let result = handle
                .join()
                .unwrap_or(Err(HostError::StagePanicked { stage }));
            self.shared.stop();
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    fn spawn(
        &mut self,
        stage: Stage,
        body: impl FnOnce() -> Result<(), HostError> + Send + 'static,
    ) -> Result<(), HostError> {
        self.spawn_on(stage, None, body)
    }

    fn spawn_on(
        &mut self,
        stage: Stage,
        stack_bytes: Option<usize>,
        body: impl FnOnce() -> Result<(), HostError> + Send + 'static,
    ) -> Result<(), HostError> {
        let mut builder = thread::Builder::new().name(format!("rusplay-host-{stage}"));
        if let Some(bytes) = stack_bytes {
            builder = builder.stack_size(bytes);
        }
        let guard = StopOnDrop(Arc::clone(&self.shared));
        let tuning = self.tuning;
        let shared = Arc::clone(&self.shared);
        let spawned = builder.spawn(move || {
            let _guard = guard;
            if let Some(role) = thread_role(stage) {
                let failures = tuning.apply(role);
                shared
                    .counters
                    .thread_priority_failures
                    .add(failures.len() as u64);
            }
            body()
        });

        match spawned {
            Ok(handle) => {
                self.stages.push((stage, handle));
                Ok(())
            }
            Err(source) => {
                self.shared.stop();
                self.join().ok();
                Err(HostError::SpawnStage { stage, source })
            }
        }
    }
}

impl Drop for HostStream {
    fn drop(&mut self) {
        self.shared.stop();
        self.join().ok();
    }
}
