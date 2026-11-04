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
use rusplay_audio::{AudioInput, CODEC_THREAD_STACK_BYTES};
use rusplay_config::{AudioConfig, Config, InputConfig};
use rusplay_core::Resolution;
use rusplay_input::{InputEvent, PadFeedback, gamepad::PhysicalOptions};
use rusplay_net::{
    PairingTiming, PeerRecord, PeerSecret, Pin, ReconnectTiming, Transport, TransportOptions,
    pair_as_client, reconnect_as_client,
};
use rusplay_priority::{ThreadRole, ThreadTuning};
use rusplay_protocol::StreamDescription;
use rusplay_scale::{GpuFaults, check_extent};

use crate::{
    AudioOutput, ClientError, ClientStats, DisplayImage, FrameSink, LatestCursor, Stage,
    audio::{self, AudioLinks, EncodedAudio},
    bitrate::BitrateSettings,
    decode::{self, DecodeLinks},
    display::resolve_target,
    input::{self, GamepadLinks, InputHandle, InputLinks, InputStage, InputTiming},
    microphone::{self, MicrophoneLinks, default_microphone},
    name::announced_name,
    present::{self, MappingOptions, PresentLinks, PresentOptions, SharedTarget},
    receive::{DecodeInput, Feedback, Links, ReceiveStage, SharedFormat},
    slot::Slot,
    stats::Counters,
};

const DECODE_STACK_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub faults: GpuFaults,
}

impl Gpu {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let faults = GpuFaults::install(&device);

        Self::with_faults(device, queue, faults)
    }

    pub const fn with_faults(device: wgpu::Device, queue: wgpu::Queue, faults: GpuFaults) -> Self {
        Self {
            device,
            queue,
            faults,
        }
    }

    pub fn guard<T>(
        &self,
        work: impl FnOnce() -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        self.faults.guard(&self.device, work)
    }

    pub fn check_extent(&self, resolution: Resolution) -> Result<(), ClientError> {
        Ok(check_extent(&self.device, resolution)?)
    }
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
            default_microphone,
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
        self.connect_with_devices(host, credentials, gpu, sink, output, None)
    }

    pub fn connect_with_devices(
        &self,
        host: SocketAddr,
        credentials: &Credentials,
        gpu: Gpu,
        sink: impl FrameSink,
        output: Option<Box<dyn AudioOutput>>,
        microphone: Option<Box<dyn AudioInput>>,
    ) -> Result<ClientStream, ClientError> {
        self.connect_stream(
            host,
            credentials,
            gpu,
            Box::new(sink),
            |_| Ok(output),
            |_| Ok(microphone),
        )
    }

    fn connect_stream(
        &self,
        host: SocketAddr,
        credentials: &Credentials,
        gpu: Gpu,
        sink: Box<dyn FrameSink>,
        open_audio: impl FnOnce(&AudioConfig) -> Result<Option<Box<dyn AudioOutput>>, ClientError>,
        open_microphone: impl FnOnce(&AudioConfig) -> Result<Option<Box<dyn AudioInput>>, ClientError>,
    ) -> Result<ClientStream, ClientError> {
        let network = &self.config.network;
        let decodable = rusplay_decode::capabilities(&self.config.video.decoder)?.formats();
        let transport = Transport::bind(
            SocketAddr::new(bind_address(network.bind_address, host), 0),
            TransportOptions::from(network),
        )?;
        let paired = authenticate(&transport, host, credentials, &self.config)?;
        let transport = Arc::new(transport);
        let (input_sender, input_events) = mpsc::channel();

        let mut stream = ClientStream {
            stages: Vec::new(),
            stop: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(Counters::default()),
            tuning: ThreadTuning::from_config(network),
            target: Arc::new(Mutex::new(None)),
            decoded: Arc::new(Slot::new()),
            format: Arc::new(Mutex::new(None)),
            cursor: LatestCursor::new(),
            input: InputHandle::new(input_sender),
            gamepads_done: Arc::new(AtomicBool::new(true)),
            paired,
        };
        let decoded = Arc::clone(&stream.decoded);
        let output = open_audio(&self.config.audio)?;

        stream.spawn_present(&self.config, gpu, sink, &decoded)?;
        let (queue, feedback) = stream.spawn_decode(&self.config, &decoded)?;
        let audio_queue = match output {
            Some(output) => Some(stream.spawn_audio(&self.config.audio, output)?),
            None => None,
        };

        let (pad_feedback, pad_feedback_commands) = mpsc::channel();

        let (handshake, handshake_done) = mpsc::channel();
        let links = Links {
            decode_queue: queue,
            audio_queue,
            pad_feedback: self.config.input.gamepads.then_some(pad_feedback),
            cursor: stream.cursor.clone(),
            format: Arc::clone(&stream.format),
            feedback,
            handshake,
            stop: Arc::clone(&stream.stop),
            counters: Arc::clone(&stream.counters),
        };
        let bitrate = BitrateSettings::from_config(network, self.config.video.bitrate_kbps);
        let receive = ReceiveStage::new(
            Arc::clone(&transport),
            host,
            decodable,
            network,
            bitrate,
            links,
        );
        stream.spawn(Stage::Receive, None, move || receive.run())?;

        let timeout = Duration::from_millis(u64::from(network.handshake_timeout_ms));
        match handshake_done.recv_timeout(timeout) {
            Ok(()) => {
                if self.config.input.gamepads {
                    stream.spawn_gamepads(&self.config.input, pad_feedback_commands)?;
                }
                if self.config.audio.microphone.enabled
                    && let Some(source) = open_microphone(&self.config.audio)?
                {
                    stream.spawn_microphone(
                        &self.config.audio,
                        source,
                        Arc::clone(&transport),
                        host,
                    )?;
                }
                stream.spawn_input(&self.config.input, transport, host, input_events)?;
                Ok(stream)
            }
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
    config: &Config,
) -> Result<Option<PeerSecret>, ClientError> {
    let network = &config.network;

    match credentials {
        Credentials::Pin(pin) => {
            let outcome = pair_as_client(
                transport,
                host,
                pin,
                announced_name(&config.peers),
                PairingTiming::from(network),
            )?;
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

const fn thread_role(stage: Stage) -> Option<ThreadRole> {
    match stage {
        Stage::Receive => Some(ThreadRole::TimerDriven),
        Stage::Decode | Stage::Present => Some(ThreadRole::Frame),
        _ => None,
    }
}

#[derive(Debug)]
pub struct ClientStream {
    stages: Vec<(Stage, JoinHandle<Result<(), ClientError>>)>,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    tuning: ThreadTuning,
    target: SharedTarget,
    decoded: Arc<Slot<decode::DecodedVideo>>,
    format: SharedFormat,
    cursor: LatestCursor,
    input: InputHandle,
    gamepads_done: Arc<AtomicBool>,
    paired: Option<PeerSecret>,
}

impl ClientStream {
    pub fn stats(&self) -> ClientStats {
        self.counters.snapshot()
    }

    pub fn set_target(&self, target: Option<Resolution>) {
        let mut current = self.target.lock();
        if *current == target {
            return;
        }

        *current = target;
        drop(current);
        self.decoded.nudge();
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

    pub fn input(&self) -> InputHandle {
        self.input.clone()
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

        let tuning = self.tuning;
        let counters = Arc::clone(&self.counters);
        let handle = builder
            .spawn(move || {
                if let Some(role) = thread_role(stage) {
                    let failures = tuning.apply(role);
                    counters.thread_priority_failures.add(failures.len() as u64);
                }
                body()
            })
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
            mapping: MappingOptions::from(&config.color),
            display: resolve_target(&DisplayImage::default(), &config.color.target)?,
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
    ) -> Result<(mpsc::SyncSender<DecodeInput>, mpsc::Receiver<Feedback>), ClientError> {
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

    fn spawn_gamepads(
        &mut self,
        config: &InputConfig,
        commands: mpsc::Receiver<PadFeedback>,
    ) -> Result<(), ClientError> {
        let options = PhysicalOptions::from(config);
        let links = GamepadLinks {
            pad_feedback: commands,
            input: self.input.clone(),
            done: Arc::clone(&self.gamepads_done),
            stop: Arc::clone(&self.stop),
            counters: Arc::clone(&self.counters),
        };
        self.gamepads_done.store(false, Ordering::Relaxed);

        self.spawn(Stage::Gamepad, None, move || {
            input::run_gamepads(options, &links);
            Ok(())
        })
    }

    fn spawn_input(
        &mut self,
        config: &InputConfig,
        transport: Arc<Transport>,
        host: SocketAddr,
        events: mpsc::Receiver<InputEvent>,
    ) -> Result<(), ClientError> {
        let links = InputLinks {
            events,
            gamepads_done: Arc::clone(&self.gamepads_done),
            stop: Arc::clone(&self.stop),
            counters: Arc::clone(&self.counters),
        };
        let timing = InputTiming {
            resend_delay: Duration::from_millis(u64::from(config.resend_delay_ms)),
            snapshot_interval: Duration::from_millis(u64::from(config.snapshot_interval_ms)),
        };
        let stage = InputStage::new(transport, host, timing, links);

        self.spawn(Stage::Input, None, move || stage.run())
    }

    fn spawn_microphone(
        &mut self,
        config: &AudioConfig,
        source: Box<dyn AudioInput>,
        transport: Arc<Transport>,
        host: SocketAddr,
    ) -> Result<(), ClientError> {
        let (ready, ready_received) = mpsc::channel();
        let config = *config;
        let links = MicrophoneLinks {
            transport,
            host,
            ready,
            stop: Arc::clone(&self.stop),
            counters: Arc::clone(&self.counters),
        };

        self.spawn(
            Stage::Microphone,
            Some(CODEC_THREAD_STACK_BYTES),
            move || microphone::run(&config, source, &links),
        )?;

        match ready_received.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.stop.store(true, Ordering::Relaxed);
                self.join().ok();
                Err(error)
            }
            Err(_) => {
                self.stop.store(true, Ordering::Relaxed);
                Err(self.join().err().unwrap_or(ClientError::StageNotReady {
                    stage: Stage::Microphone,
                }))
            }
        }
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

#[cfg(test)]
mod tests {
    use rusplay_scale::GpuFault;

    use super::*;
    use crate::testing;

    #[test]
    fn a_resolution_past_the_device_limit_is_a_client_error() {
        let Some(gpu) = testing::gpu() else {
            return;
        };
        let limit = gpu.device.limits().max_texture_dimension_2d;

        let refused = gpu.check_extent(Resolution::new(limit + 1, 1));
        let allowed = gpu.check_extent(Resolution::new(limit, limit));

        assert!(matches!(
            refused,
            Err(ClientError::Gpu(GpuFault::TextureTooLarge { .. }))
        ));
        assert!(allowed.is_ok());
    }

    #[test]
    fn a_guarded_gpu_error_is_a_client_error_not_an_abort() {
        let Some(gpu) = testing::gpu() else {
            return;
        };
        let side = gpu.device.limits().max_texture_dimension_2d + 1;

        let result = gpu.guard(|| {
            drop(gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rusplay-test-oversized"),
                size: wgpu::Extent3d {
                    width: side,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            }));
            Ok(())
        });

        assert!(matches!(
            result,
            Err(ClientError::Gpu(GpuFault::Validation(_)))
        ));
    }
}
