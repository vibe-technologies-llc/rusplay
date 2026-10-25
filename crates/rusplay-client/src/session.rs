use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use parking_lot::Mutex;
use rusplay_audio::CODEC_THREAD_STACK_BYTES;
use rusplay_config::{AudioConfig, Config, NetworkConfig};
use rusplay_core::Resolution;
use rusplay_net::{
    PairingTiming, PeerRecord, PeerSecret, Pin, ReconnectTiming, Transport, TransportOptions,
    pair_as_client, reconnect_as_client,
};
use rusplay_protocol::StreamDescription;

use crate::{
    AudioOutput, ClientError, ClientStats, FrameSink, LatestCursor, Stage,
    audio::{self, AudioLinks, EncodedAudio},
    decode::{self, DecodeLinks},
    present::{self, PresentLinks, PresentOptions, SharedTarget},
    receive::{EncodedVideo, Feedback, Links, ReceiveStage, SharedFormat},
    slot::Slot,
    stats::Counters,
};

const DECODE_STACK_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

#[derive(Debug, Clone)]
pub enum Credentials {
    Pin(Pin),
    Peer(PeerRecord),
}

#[derive(Debug)]
pub struct ClientSession {
    config: Config,
}

impl ClientSession {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn probe(&self) -> Result<(), ClientError> {
        let _decoder = rusplay_decode::open(self.config.video.codec, &self.config.video.decoder)?;
        let _audio = audio::open_decoder(&self.config.audio)?;

        Ok(())
    }

    pub fn connect(
        &self,
        host: SocketAddr,
        credentials: &Credentials,
        gpu: Gpu,
        sink: impl FrameSink,
    ) -> Result<ClientStream, ClientError> {
        self.connect_stream(
            host,
            credentials,
            gpu,
            Box::new(sink),
            audio::default_audio_output,
        )
    }

    pub fn connect_with_audio(
        &self,
        host: SocketAddr,
        credentials: &Credentials,
        gpu: Gpu,
        sink: impl FrameSink,
        output: Option<Box<dyn AudioOutput>>,
    ) -> Result<ClientStream, ClientError> {
        self.connect_stream(host, credentials, gpu, Box::new(sink), |_| Ok(output))
    }

    fn connect_stream(
        &self,
        host: SocketAddr,
        credentials: &Credentials,
        gpu: Gpu,
        sink: Box<dyn FrameSink>,
        open_audio: impl FnOnce(&AudioConfig) -> Result<Option<Box<dyn AudioOutput>>, ClientError>,
    ) -> Result<ClientStream, ClientError> {
        let network = &self.config.network;
        let transport = Transport::bind(
            SocketAddr::new(bind_address(network.bind_address, host), 0),
            TransportOptions::from(network),
        )?;
        let paired = authenticate(&transport, host, credentials, network)?;

        let mut stream = ClientStream {
            stages: Vec::new(),
            stop: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(Counters::default()),
            target: Arc::new(Mutex::new(None)),
            format: Arc::new(Mutex::new(None)),
            cursor: LatestCursor::new(),
            paired,
        };
        let decoded = Arc::new(Slot::new());
        let output = open_audio(&self.config.audio)?;

        stream.spawn_present(&self.config, gpu, sink, &decoded)?;
        let (queue, feedback) = stream.spawn_decode(&self.config, &decoded)?;
        let audio_queue = match output {
            Some(output) => Some(stream.spawn_audio(&self.config.audio, output)?),
            None => None,
        };

        let (handshake, handshake_done) = mpsc::channel();
        let links = Links {
            decode_queue: queue,
            audio_queue,
            cursor: stream.cursor.clone(),
            format: Arc::clone(&stream.format),
            feedback,
            handshake,
            stop: Arc::clone(&stream.stop),
            counters: Arc::clone(&stream.counters),
        };
        let receive = ReceiveStage::new(transport, host, network, links);
        stream.spawn(Stage::Receive, None, move || receive.run())?;

        let timeout = Duration::from_millis(u64::from(network.handshake_timeout_ms));
        match handshake_done.recv_timeout(timeout) {
            Ok(()) => Ok(stream),
            Err(RecvTimeoutError::Timeout) => {
                stream.stop().ok();
                Err(ClientError::HandshakeTimeout(timeout))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(stream.wait().err().unwrap_or(ClientError::HandshakeAborted))
            }
        }
    }
}

fn bind_address(configured: IpAddr, host: SocketAddr) -> IpAddr {
    match (configured, host) {
        (configured, host) if configured.is_ipv4() == host.is_ipv4() => configured,
        (_, SocketAddr::V4(_)) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        (_, SocketAddr::V6(_)) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    }
}

fn authenticate(
    transport: &Transport,
    host: SocketAddr,
    credentials: &Credentials,
    network: &NetworkConfig,
) -> Result<Option<PeerSecret>, ClientError> {
    match credentials {
        Credentials::Pin(pin) => {
            let outcome = pair_as_client(transport, host, pin, PairingTiming::from(network))?;
            transport.install_session(&outcome.keys);
            Ok(Some(outcome.secret))
        }
        Credentials::Peer(record) => {
            let keys =
                reconnect_as_client(transport, host, record, ReconnectTiming::from(network))?;
            transport.install_session(&keys);
            Ok(None)
        }
    }
}

#[derive(Debug)]
pub struct ClientStream {
    stages: Vec<(Stage, JoinHandle<Result<(), ClientError>>)>,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    target: SharedTarget,
    format: SharedFormat,
    cursor: LatestCursor,
    paired: Option<PeerSecret>,
}

impl ClientStream {
    pub fn stats(&self) -> ClientStats {
        self.counters.snapshot()
    }

    pub fn set_target(&self, target: Option<Resolution>) {
        *self.target.lock() = target;
    }

    pub fn target(&self) -> Option<Resolution> {
        *self.target.lock()
    }

    pub fn stream_format(&self) -> Option<StreamDescription> {
        *self.format.lock()
    }

    pub fn cursor(&self) -> LatestCursor {
        self.cursor.clone()
    }

    pub fn paired_secret(&self) -> Option<&PeerSecret> {
        self.paired.as_ref()
    }

    pub fn is_finished(&self) -> bool {
        self.stages.iter().any(|(_, handle)| handle.is_finished())
    }

    pub fn stop(mut self) -> Result<(), ClientError> {
        self.stop.store(true, Ordering::Relaxed);
        self.join()
    }

    pub fn wait(mut self) -> Result<(), ClientError> {
        self.join()
    }

    fn join(&mut self) -> Result<(), ClientError> {
        let mut first_error = None;

        for (stage, handle) in self.stages.drain(..) {
            let result = handle
                .join()
                .unwrap_or(Err(ClientError::StagePanicked { stage }));
            self.stop.store(true, Ordering::Relaxed);
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    fn spawn(
        &mut self,
        stage: Stage,
        stack_bytes: Option<usize>,
        body: impl FnOnce() -> Result<(), ClientError> + Send + 'static,
    ) -> Result<(), ClientError> {
        let builder = thread::Builder::new().name(format!("rusplay-client-{stage}"));
        let builder = match stack_bytes {
            Some(bytes) => builder.stack_size(bytes),
            None => builder,
        };

        let handle = builder
            .spawn(body)
            .map_err(|source| ClientError::SpawnStage { stage, source })?;
        self.stages.push((stage, handle));

        Ok(())
    }

    fn spawn_present(
        &mut self,
        config: &Config,
        gpu: Gpu,
        sink: Box<dyn FrameSink>,
        decoded: &Arc<Slot<decode::DecodedVideo>>,
    ) -> Result<(), ClientError> {
        let options = PresentOptions {
            scaling: config.scaling,
        };
        let links = PresentLinks {
            input: Arc::clone(decoded),
            target: Arc::clone(&self.target),
            sink,
            stop: Arc::clone(&self.stop),
            counters: Arc::clone(&self.counters),
        };

        self.spawn(Stage::Present, None, move || {
            present::run(gpu, options, links)
        })
    }

    fn spawn_decode(
        &mut self,
        config: &Config,
        decoded: &Arc<Slot<decode::DecodedVideo>>,
    ) -> Result<(mpsc::SyncSender<EncodedVideo>, mpsc::Receiver<Feedback>), ClientError> {
        let (queue, queued) = mpsc::sync_channel(usize::from(config.video.decode_queue_frames));
        let (feedback, feedback_received) = mpsc::channel();
        let backend = config.video.decoder.clone();
        let links = DecodeLinks {
            queue: queued,
            output: Arc::clone(decoded),
            feedback,
            stop: Arc::clone(&self.stop),
            counters: Arc::clone(&self.counters),
        };

        self.spawn(Stage::Decode, Some(DECODE_STACK_BYTES), move || {
            decode::run(&backend, &links)
        })?;

        Ok((queue, feedback_received))
    }

    fn spawn_audio(
        &mut self,
        config: &AudioConfig,
        output: Box<dyn AudioOutput>,
    ) -> Result<mpsc::SyncSender<EncodedAudio>, ClientError> {
        let (queue, queued) = mpsc::sync_channel(usize::from(config.decode_queue_packets));
        let (ready, ready_received) = mpsc::channel();
        let config = *config;
        let links = AudioLinks {
            queue: queued,
            ready,
            stop: Arc::clone(&self.stop),
            counters: Arc::clone(&self.counters),
        };

        self.spawn(Stage::Audio, Some(CODEC_THREAD_STACK_BYTES), move || {
            audio::run(&config, output, &links)
        })?;

        match ready_received.recv() {
            Ok(Ok(())) => Ok(queue),
            Ok(Err(error)) => {
                self.stop.store(true, Ordering::Relaxed);
                self.join().ok();
                Err(error)
            }
            Err(_) => {
                self.stop.store(true, Ordering::Relaxed);
                Err(self.join().err().unwrap_or(ClientError::StageNotReady {
                    stage: Stage::Audio,
                }))
            }
        }
    }
}

impl Drop for ClientStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.join().ok();
    }
}
