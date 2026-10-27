#![allow(dead_code)]

use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_audio::{AudioEncoder, CODEC_THREAD_STACK_BYTES, OpusAudioEncoder};
use rusplay_client::{
    AudioOutput, AudioOutputStats, ClientError, ClientSession, ClientStream, Credentials,
    FrameSink, Gpu, Pin,
};
use rusplay_config::{Config, NetworkConfig, SoftwareDecoderConfig, VideoDecoderConfig};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, CodecFormat, ColorPrimaries, ColorRange, ColorSpace,
    FormatSet, PixelFormat, Resolution, TransferFunction,
};
use rusplay_input::{InputEvent, InputRecord, StateRecord};
use rusplay_net::{
    ControlChannel, ControlConfig, ControlEvent, FecScheme, Packetizer, PairingResponder,
    PairingTiming, Transport, TransportOptions, pair_as_host,
};
use rusplay_protocol::{
    ControlMessage, CursorMessage, InputAdmission, InputPacket, InputSequence, PacketHeader,
    ProtocolVersion, StreamDescription, StreamKind,
};

pub const PIN: &str = "482916";
pub const FIXTURE_WIDTH: u32 = 256;
pub const FIXTURE_HEIGHT: u32 = 144;

pub const DEFAULT_FRAMES: u32 = 24;
const IVF_HEADER_LEN: usize = 32;
const IVF_FRAME_HEADER_LEN: usize = 12;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const TEST_DECODE_QUEUE_FRAMES: u8 = u8::MAX;
const AUDIO_INTERVAL: Duration = Duration::from_millis(5);
const AUDIO_PACKET_BUFFER: usize = 4000;
const POLL_INTERVAL: Duration = Duration::from_millis(1);
const FEC_DATA_SHARDS: u8 = 32;
const HOST_PAIRING_TIMEOUT_MS: u32 = 3_000;
const GOODBYE_LINGER: Duration = Duration::from_millis(200);

pub fn gpu() -> Option<Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();

    GPU.get_or_init(|| pollster::block_on(request_gpu()))
        .clone()
}

async fn request_gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..wgpu::RequestAdapterOptions::default()
        })
        .await
        .ok()?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .ok()?;

    Some(Gpu { device, queue })
}

pub fn stream_description() -> StreamDescription {
    StreamDescription {
        generation: 1,
        codec: Codec::Av1,
        resolution: Resolution::new(FIXTURE_WIDTH, FIXTURE_HEIGHT),
        format: PixelFormat {
            chroma: ChromaSubsampling::Yuv420,
            depth: BitDepth::Eight,
        },
        color: ColorSpace {
            primaries: ColorPrimaries::DisplayP3,
            transfer: TransferFunction::Srgb,
            range: ColorRange::Full,
            ..ColorSpace::default()
        },
        configured: CodecFormat::new(
            Codec::Hevc,
            PixelFormat {
                chroma: ChromaSubsampling::Yuv422,
                depth: BitDepth::Eight,
            },
        ),
    }
}

pub fn client_config() -> Config {
    let mut config = Config::default();
    config.video.codec = Codec::H264;
    config.video.decoder = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());
    config.video.decode_queue_frames = TEST_DECODE_QUEUE_FRAMES;
    config.network.bind_address = IpAddr::V4(Ipv4Addr::LOCALHOST);
    config.input.gamepads = false;

    config
}

pub fn pin() -> Pin {
    Pin::parse(PIN).unwrap()
}

pub fn fixture_frames() -> Vec<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../rusplay-decode/tests/data/av1-420-8bit.ivf");
    let file =
        fs::read(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    assert_eq!(&file[..4], b"DKIF");

    let mut frames = Vec::new();
    let mut cursor = IVF_HEADER_LEN;
    while cursor + IVF_FRAME_HEADER_LEN <= file.len() {
        let size = u32::from_le_bytes(file[cursor..cursor + 4].try_into().unwrap());
        let start = cursor + IVF_FRAME_HEADER_LEN;
        let end = start + usize::try_from(size).unwrap();
        frames.push(file[start..end].to_vec());
        cursor = end;
    }

    frames
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outgoing {
    pub frame: u32,
    pub index: usize,
    pub parity: bool,
}

pub type DropRule = Box<dyn Fn(Outgoing) -> bool + Send>;

pub struct HostOptions {
    pub frames: u32,
    pub drop: DropRule,
    pub goodbye: bool,
    pub audio: Vec<Vec<u8>>,
    pub audio_drop: Box<dyn Fn(u32) -> bool + Send>,
    pub cursor: Vec<CursorMessage>,
    pub gate: Option<Arc<AtomicBool>>,
    pub encodable: FormatSet,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            frames: DEFAULT_FRAMES,
            drop: Box::new(|_| false),
            goodbye: true,
            audio: Vec::new(),
            audio_drop: Box::new(|_| false),
            cursor: Vec::new(),
            gate: None,
            encodable: FormatSet::ALL,
        }
    }
}

pub struct TestHost {
    pub address: SocketAddr,
    pub received: Arc<Mutex<Vec<ControlMessage>>>,
    pub input: Arc<Mutex<Vec<InputRecord>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestHost {
    pub fn start(options: HostOptions) -> Self {
        let network = NetworkConfig {
            pairing_timeout_ms: HOST_PAIRING_TIMEOUT_MS,
            ..NetworkConfig::default()
        };
        let transport = Transport::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            TransportOptions::from(&network),
        )
        .unwrap();
        let address = transport.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let input = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let received = Arc::clone(&received);
            let input = Arc::clone(&input);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                serve(&transport, &network, &options, &received, &input, &stop);
            })
        };

        Self {
            address,
            received,
            input,
            stop,
            thread: Some(thread),
        }
    }

    pub fn messages(&self) -> Vec<ControlMessage> {
        self.received.lock().clone()
    }

    pub fn input_events(&self) -> Vec<InputEvent> {
        self.input
            .lock()
            .iter()
            .filter_map(|record| match record {
                InputRecord::Event(event) => Some(*event),
                InputRecord::State(_) => None,
            })
            .collect()
    }

    pub fn state_records(&self) -> Vec<StateRecord> {
        self.input
            .lock()
            .iter()
            .filter_map(|record| match record {
                InputRecord::State(state) => Some(*state),
                InputRecord::Event(_) => None,
            })
            .collect()
    }

    pub fn wait_for_state(&self, timeout: Duration, wanted: impl Fn(&StateRecord) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.state_records().iter().any(&wanted) {
                return true;
            }
            thread::sleep(POLL_INTERVAL);
        }
        false
    }

    pub fn wait_for_input(&self, timeout: Duration, count: usize) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.input_events().len() >= count {
                return true;
            }
            thread::sleep(POLL_INTERVAL);
        }
        false
    }

    pub fn wait_for(&self, timeout: Duration, matches: impl Fn(&ControlMessage) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.received.lock().iter().any(&matches) {
                return true;
            }
            thread::sleep(POLL_INTERVAL);
        }
        false
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

struct Extras {
    audio: Packetizer,
    cursor: Packetizer,
    audio_index: usize,
    audio_next_send: Instant,
    cursor_index: usize,
    cursor_next_send: Instant,
}

impl Extras {
    fn new(transport: &Transport, client: &SocketAddr) -> Self {
        let packetizer = |stream| {
            Packetizer::new(stream, transport.max_datagram_len(client), FecScheme::None).unwrap()
        };

        Self {
            audio: packetizer(StreamKind::Audio),
            cursor: packetizer(StreamKind::Cursor),
            audio_index: 0,
            audio_next_send: Instant::now(),
            cursor_index: 0,
            cursor_next_send: Instant::now(),
        }
    }

    fn done(&self, options: &HostOptions) -> bool {
        self.audio_index == options.audio.len() && self.cursor_index == options.cursor.len()
    }

    fn send_due(&mut self, transport: &Transport, client: SocketAddr, options: &HostOptions) {
        let now = Instant::now();

        if self.audio_index < options.audio.len() && now >= self.audio_next_send {
            let number = u32::try_from(self.audio_index).unwrap();
            if !(options.audio_drop)(number) {
                let datagrams = self
                    .audio
                    .packetize(number, false, &options.audio[self.audio_index])
                    .unwrap();
                for datagram in datagrams {
                    transport.send_to(&datagram, client).unwrap();
                }
            }
            self.audio_index += 1;
            self.audio_next_send += AUDIO_INTERVAL;
        }

        if self.cursor_index < options.cursor.len() && now >= self.cursor_next_send {
            let mut encoded = Vec::new();
            options.cursor[self.cursor_index]
                .encode(&mut encoded)
                .unwrap();
            let number = u32::try_from(self.cursor_index).unwrap();
            for datagram in self.cursor.packetize(number, false, &encoded).unwrap() {
                transport.send_to(&datagram, client).unwrap();
            }
            self.cursor_index += 1;
            self.cursor_next_send += FRAME_INTERVAL;
        }
    }
}

fn record_input(datagram: &[u8], sequence: &mut InputSequence, input: &Mutex<Vec<InputRecord>>) {
    let packet = InputPacket::read(datagram).unwrap();
    let decode = |records: rusplay_protocol::InputRecords<'_>| {
        records
            .map(|record| InputRecord::decode(record.unwrap()).unwrap())
            .collect::<Vec<_>>()
    };

    let mut input = input.lock();
    match sequence.admit(packet.sequence) {
        InputAdmission::Stale => {}
        InputAdmission::Current => input.extend(decode(packet.current_records())),
        InputAdmission::PreviousAndCurrent => {
            input.extend(decode(packet.previous_records()));
            input.extend(decode(packet.current_records()));
        }
    }
}

fn answer_hello(
    control: &mut ControlChannel,
    transport: &Transport,
    client: SocketAddr,
    options: &HostOptions,
    describe: bool,
) {
    let hello = ControlMessage::Hello {
        version: ProtocolVersion::CURRENT,
        formats: options.encodable,
    };

    control.send(hello, Instant::now()).ok();
    if describe {
        control
            .send(
                ControlMessage::StreamFormat(stream_description()),
                Instant::now(),
            )
            .ok();
    }
    control.flush(transport, client).ok();
}

fn gate_open(options: &HostOptions) -> bool {
    options
        .gate
        .as_ref()
        .is_none_or(|gate| gate.load(Ordering::Acquire))
}

fn serve(
    transport: &Transport,
    network: &NetworkConfig,
    options: &HostOptions,
    received: &Mutex<Vec<ControlMessage>>,
    input: &Mutex<Vec<InputRecord>>,
    stop: &AtomicBool,
) {
    let mut responder = PairingResponder::new(pin(), network.pairing_max_attempts);
    let Ok((client, outcome)) =
        pair_as_host(transport, &mut responder, PairingTiming::from(network))
    else {
        return;
    };
    transport.install_session(&outcome.keys);

    let fixture = fixture_frames();
    let mut control = ControlChannel::new(ControlConfig::from(network));
    let mut packetizer = Packetizer::new(
        StreamKind::Video,
        transport.max_datagram_len(&client),
        FecScheme::for_overhead(FEC_DATA_SHARDS, network.fec_overhead_percent),
    )
    .unwrap();
    let mut extras = Extras::new(transport, &client);
    let mut input_sequence = InputSequence::new();
    let mut events = Vec::new();
    let mut buf = vec![0; usize::from(network.mtu)];
    let mut streaming = false;
    let mut next_frame = 0_u32;
    let mut fixture_index = 0_usize;
    let mut next_send = Instant::now();
    let mut finished_at: Option<Instant> = None;
    let mut live = false;

    while !stop.load(Ordering::Relaxed) {
        if let Ok(Some((len, from))) = transport.recv_from_timeout(&mut buf, POLL_INTERVAL)
            && from == client
        {
            match PacketHeader::read(&buf[..len]).map(|header| header.stream) {
                Ok(StreamKind::Control) => {
                    control
                        .handle_datagram(&buf[..len], Instant::now(), &mut events)
                        .ok();
                }
                Ok(StreamKind::Input) => record_input(&buf[..len], &mut input_sequence, input),
                _ => {}
            }
        }
        control.poll(Instant::now(), &mut events);

        for event in events.drain(..) {
            let ControlEvent::Received(message) = event else {
                continue;
            };
            received.lock().push(message);
            match message {
                ControlMessage::Hello { .. } => {
                    answer_hello(&mut control, transport, client, options, !streaming);
                    streaming = true;
                }
                ControlMessage::RequestKeyframe { .. } => fixture_index = 0,
                _ => {}
            }
        }

        if streaming && !live && gate_open(options) {
            live = true;
            next_send = Instant::now();
        }

        if live && next_frame < options.frames && Instant::now() >= next_send {
            let keyframe = fixture_index == 0;
            let datagrams = packetizer
                .packetize(next_frame, keyframe, &fixture[fixture_index])
                .unwrap();
            let mut data_index = 0;
            let mut parity_index = 0;
            for datagram in &datagrams {
                let parity = PacketHeader::read(datagram).unwrap().stream == StreamKind::Fec;
                let index = if parity {
                    &mut parity_index
                } else {
                    &mut data_index
                };
                let outgoing = Outgoing {
                    frame: next_frame,
                    index: *index,
                    parity,
                };
                *index += 1;
                if !(options.drop)(outgoing) {
                    transport.send_to(datagram, client).unwrap();
                }
            }

            next_frame += 1;
            fixture_index = (fixture_index + 1) % fixture.len();
            next_send += FRAME_INTERVAL;
        }

        if live {
            extras.send_due(transport, client, options);
        }

        let media_done = next_frame == options.frames && extras.done(options);
        if media_done && options.goodbye && finished_at.is_none() {
            control.send(ControlMessage::Goodbye, Instant::now()).ok();
            finished_at = Some(Instant::now());
        }
        if finished_at.is_some_and(|at| at.elapsed() >= GOODBYE_LINGER) {
            control.flush(transport, client).ok();
            return;
        }

        control.flush(transport, client).ok();
    }
}

pub fn download(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<[u16; 4]> {
    let (width, height) = (texture.width(), texture.height());
    let texel_bytes = 8;
    let row_bytes = (width * texel_bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rusplay-client-test-readback"),
        size: u64::from(row_bytes * height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);

    staging.map_async(wgpu::MapMode::Read, .., |result| {
        result.expect("readback buffer maps");
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device finishes the readback");
    let bytes = staging
        .get_mapped_range(..)
        .expect("mapped range is readable")
        .to_vec();

    bytes
        .chunks_exact(row_bytes as usize)
        .flat_map(|row| {
            row[..(width * texel_bytes) as usize]
                .as_chunks::<8>()
                .0
                .to_vec()
        })
        .map(|texel| {
            std::array::from_fn(|channel| {
                u16::from_le_bytes([texel[2 * channel], texel[2 * channel + 1]])
            })
        })
        .collect()
}

pub fn opus_packets(count: u32) -> Vec<Vec<u8>> {
    thread::Builder::new()
        .stack_size(CODEC_THREAD_STACK_BYTES)
        .spawn(move || {
            let config = rusplay_config::AudioConfig::default();
            let mut encoder = OpusAudioEncoder::new(&config).unwrap();
            let samples = encoder.frame_samples() * usize::from(config.channels.channel_count());

            (0..count)
                .map(|number| {
                    let tone: Vec<f32> = (0..samples)
                        .map(|index| {
                            let phase = f32::from(u16::try_from(index).unwrap())
                                + f32::from(u16::try_from(number).unwrap()) * 7.0;
                            (phase * 0.05).sin() * 0.5
                        })
                        .collect();
                    let mut packet = vec![0; AUDIO_PACKET_BUFFER];
                    let written = encoder.encode(&tone, &mut packet).unwrap();
                    packet.truncate(written);
                    packet
                })
                .collect()
        })
        .unwrap()
        .join()
        .unwrap()
}

#[derive(Clone, Default)]
pub struct CapturedAudio {
    pub samples: Arc<Mutex<Vec<f32>>>,
}

impl AudioOutput for CapturedAudio {
    fn submit(&mut self, pcm: &[f32]) -> Result<usize, rusplay_audio::AudioError> {
        self.samples.lock().extend_from_slice(pcm);
        Ok(0)
    }

    fn stats(&self) -> AudioOutputStats {
        AudioOutputStats::default()
    }
}

pub fn connect_silent(
    session: &ClientSession,
    host: SocketAddr,
    credentials: &Credentials,
    gpu: Gpu,
    sink: impl FrameSink,
) -> Result<ClientStream, ClientError> {
    session.connect_with_audio(host, credentials, gpu, sink, None)
}
