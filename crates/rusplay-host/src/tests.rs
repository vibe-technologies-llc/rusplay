mod input;
mod media;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use rusplay_capture::{CaptureError, CursorUpdate, DrmFourcc, Frame, FrameBuffer, Subscriber};
use rusplay_config::{Config, NetworkConfig};
use rusplay_core::{Codec, ColorSpace, EncodedFrame, PixelFormat, Resolution, Timestamp};
use rusplay_net::{
    ControlChannel, ControlConfig, ControlEvent, NetError, PairingTiming, PeerRecord, PeerStore,
    Pin, ReassembledFrame, Reassembler, ReassemblyEvent, ReassemblyLimits, ReconnectTiming,
    Transport, TransportOptions, pair_as_client, reconnect_as_client,
};
use rusplay_protocol::{
    ClientName, ControlMessage, CursorMessage, FecHeader, PacketHeader, PairingRejection,
    ProtocolVersion, ReconnectRejection, StreamDescription, StreamKind,
};
use tempfile::TempDir;

use crate::{
    Admission, CaptureFeed, FrameSource, HostError, HostSession, HostStream,
    encode::PipelineFactory,
    pipeline::{StreamFormat, VideoPipeline},
};

const PIN: &str = "583014";
const CLIENT_NAME: &str = "test client";
const FRAME_SIZE: Resolution = Resolution::new(64, 32);
const RESIZED: Resolution = Resolution::new(96, 48);
const FRAME_BYTES: usize = 5_000;
const SOURCE_INTERVAL: Duration = Duration::from_millis(10);
const WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(2);
const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

thread_local! {
    static STREAM_STARTED: Arc<AtomicBool> = Arc::default();
}

fn stream_started() -> Arc<AtomicBool> {
    STREAM_STARTED.with(Arc::clone)
}

fn network() -> NetworkConfig {
    NetworkConfig {
        bind_address: LOCALHOST,
        port: 0,
        pairing_timeout_ms: 5_000,
        handshake_timeout_ms: 500,
        peer_timeout_ms: 800,
        keepalive_interval_ms: 100,
        ..NetworkConfig::default()
    }
}

fn host_config() -> Config {
    Config {
        network: network(),
        ..Config::default()
    }
}

fn pin() -> Pin {
    Pin::parse(PIN).unwrap()
}

struct CountingSource {
    remaining: u32,
    produced: u32,
    resize_after: Option<u32>,
    cursor: Option<Subscriber<CursorUpdate>>,
    started: Arc<AtomicBool>,
}

impl FrameSource for CountingSource {
    fn cursor_feed(&self) -> Option<Subscriber<CursorUpdate>> {
        self.cursor.clone()
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        if self.remaining == 0 || !self.started.load(Ordering::Acquire) {
            thread::sleep(timeout);
            return Ok(None);
        }

        thread::sleep(SOURCE_INTERVAL.min(timeout));
        self.remaining -= 1;
        self.produced += 1;
        let resolution = match self.resize_after {
            Some(after) if self.produced > after => RESIZED,
            _ => FRAME_SIZE,
        };
        let stride = resolution.width * 4;
        let fill = u8::try_from(self.produced % 256).unwrap();

        Ok(Some(Frame {
            resolution,
            format: DrmFourcc::from_code(*b"XR24"),
            color: ColorSpace::default(),
            timestamp: Timestamp::from_micros(u64::from(self.produced) * 10_000),
            buffer: FrameBuffer::Shm {
                data: vec![fill; (stride * resolution.height) as usize],
                stride,
            },
        }))
    }
}

fn feed(frames: u32) -> Arc<CaptureFeed> {
    CaptureFeed::new(CountingSource {
        remaining: frames,
        produced: 0,
        resize_after: None,
        cursor: None,
        started: stream_started(),
    })
}

fn resizing_feed(frames: u32, after: u32) -> Arc<CaptureFeed> {
    CaptureFeed::new(CountingSource {
        remaining: frames,
        produced: 0,
        resize_after: Some(after),
        cursor: None,
        started: stream_started(),
    })
}

fn resizing_feed_with_cursor(
    frames: u32,
    after: u32,
    cursor: Subscriber<CursorUpdate>,
) -> Arc<CaptureFeed> {
    CaptureFeed::new(CountingSource {
        remaining: frames,
        produced: 0,
        resize_after: Some(after),
        cursor: Some(cursor),
        started: stream_started(),
    })
}

fn feed_with_cursor(frames: u32, cursor: Subscriber<CursorUpdate>) -> Arc<CaptureFeed> {
    CaptureFeed::new(CountingSource {
        remaining: frames,
        produced: 0,
        resize_after: None,
        cursor: Some(cursor),
        started: stream_started(),
    })
}

#[derive(Default)]
struct FakePipeline {
    last: Option<u8>,
    encoded: u32,
    resolution: Option<Resolution>,
    stream: Option<Resolution>,
    bitrate: Arc<AtomicU32>,
}

impl FakePipeline {
    fn emit(&mut self, fill: u8, keyframe: bool, timestamp: Timestamp) -> EncodedFrame {
        let keyframe = keyframe || self.encoded == 0;
        self.encoded += 1;

        EncodedFrame {
            codec: Codec::Av1,
            timestamp,
            keyframe,
            data: vec![fill; FRAME_BYTES],
        }
    }
}

impl VideoPipeline for FakePipeline {
    fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Option<EncodedFrame>, HostError> {
        let FrameBuffer::Shm { data, .. } = &frame.buffer else {
            return Ok(None);
        };
        self.last = Some(data[0]);
        let resized = self.resolution.is_some_and(|size| size != frame.resolution);
        self.resolution = Some(frame.resolution);

        Ok(Some(self.emit(
            data[0],
            keyframe || resized,
            frame.timestamp,
        )))
    }

    fn repeat(
        &mut self,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, HostError> {
        Ok(self.last.map(|fill| self.emit(fill, keyframe, timestamp)))
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), HostError> {
        self.bitrate.store(kbps, Ordering::Relaxed);
        Ok(())
    }

    fn stream_format(&self) -> Option<StreamFormat> {
        self.resolution.map(|resolution| StreamFormat {
            codec: Codec::Av1,
            resolution: self.stream.unwrap_or(resolution),
            format: PixelFormat::default(),
            color: ColorSpace::default(),
        })
    }
}

fn fake_factory(bitrate: &Arc<AtomicU32>) -> PipelineFactory {
    let bitrate = Arc::clone(bitrate);
    Box::new(move || {
        Ok(Box::new(FakePipeline {
            bitrate,
            ..FakePipeline::default()
        }) as Box<dyn VideoPipeline>)
    })
}

fn scaling_factory(bitrate: &Arc<AtomicU32>, stream: Resolution) -> PipelineFactory {
    let bitrate = Arc::clone(bitrate);
    Box::new(move || {
        Ok(Box::new(FakePipeline {
            bitrate,
            stream: Some(stream),
            ..FakePipeline::default()
        }) as Box<dyn VideoPipeline>)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReceivedFrame {
    number: u32,
    keyframe: bool,
    fill: u8,
}

struct TestClient {
    transport: Transport,
    host: SocketAddr,
    control: ControlChannel,
    video: Reassembler,
    audio: Reassembler,
    cursor: Reassembler,
    frames: Vec<ReceivedFrame>,
    audio_packets: Vec<ReassembledFrame>,
    cursor_messages: Vec<CursorMessage>,
    messages: Vec<ControlMessage>,
    formats: Vec<(usize, StreamDescription)>,
    stream_started: Arc<AtomicBool>,
}

impl TestClient {
    fn bind(host: SocketAddr) -> Self {
        let network = network();
        let transport = Transport::bind(
            SocketAddr::new(LOCALHOST, 0),
            TransportOptions::from(&network),
        )
        .unwrap();

        Self {
            transport,
            host,
            control: ControlChannel::new(ControlConfig::from(&network)),
            video: Reassembler::new(StreamKind::Video, ReassemblyLimits::from(&network)),
            audio: Reassembler::new(StreamKind::Audio, ReassemblyLimits::from(&network)),
            cursor: Reassembler::new(StreamKind::Cursor, ReassemblyLimits::from(&network)),
            frames: Vec::new(),
            audio_packets: Vec::new(),
            cursor_messages: Vec::new(),
            messages: Vec::new(),
            formats: Vec::new(),
            stream_started: stream_started(),
        }
    }

    fn pair(host: SocketAddr) -> (Self, PeerRecord) {
        let client = Self::bind(host);
        let outcome = pair_as_client(
            &client.transport,
            host,
            &pin(),
            ClientName::new(CLIENT_NAME).unwrap(),
            PairingTiming::from(&network()),
        )
        .unwrap();
        client.transport.install_session(&outcome.keys);

        (
            client,
            PeerRecord::new("test host", outcome.secret).unwrap(),
        )
    }

    fn reconnect(host: SocketAddr, record: &PeerRecord) -> Self {
        let client = Self::bind(host);
        let keys = reconnect_as_client(
            &client.transport,
            host,
            record,
            ReconnectTiming::from(&network()),
        )
        .unwrap();
        client.transport.install_session(&keys);

        client
    }

    fn send(&mut self, message: ControlMessage) {
        self.control.send(message, Instant::now()).unwrap();
        self.control.flush(&self.transport, self.host).unwrap();
    }

    fn send_datagram(&self, datagram: &[u8]) {
        self.transport.send_to(datagram, self.host).unwrap();
    }

    fn hello(&mut self) {
        self.send(ControlMessage::Hello {
            version: ProtocolVersion::CURRENT,
        });
    }

    fn pump_until(&mut self, done: impl Fn(&Self) -> bool) -> bool {
        self.pump(WAIT, done)
    }

    fn pump(&mut self, timeout: Duration, done: impl Fn(&Self) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let mut buf = vec![0; usize::from(network().mtu)];
        let mut events = Vec::new();
        let mut control_events = Vec::new();

        while !done(self) {
            if Instant::now() >= deadline {
                return false;
            }

            if let Some((len, from)) = self.transport.recv_from_timeout(&mut buf, POLL).unwrap()
                && from == self.host
            {
                self.route(&buf[..len], &mut events, &mut control_events);
            }
            self.control.poll(Instant::now(), &mut control_events);

            for event in events.drain(..) {
                if let ReassemblyEvent::Frame(frame) = event {
                    self.frames.push(ReceivedFrame {
                        number: frame.frame,
                        keyframe: frame.keyframe,
                        fill: frame.data[0],
                    });
                }
            }
            for event in control_events.drain(..) {
                if let ControlEvent::Received(message) = event {
                    if matches!(message, ControlMessage::Hello { .. }) {
                        self.stream_started.store(true, Ordering::Release);
                    }
                    if let ControlMessage::StreamFormat(description) = message {
                        self.formats.push((self.frames.len(), description));
                    }
                    self.messages.push(message);
                }
            }
            self.control.flush(&self.transport, self.host).unwrap();
        }

        true
    }

    fn route(
        &mut self,
        datagram: &[u8],
        events: &mut Vec<ReassemblyEvent>,
        control_events: &mut Vec<ControlEvent>,
    ) {
        let Ok((header, payload)) = PacketHeader::parse(datagram) else {
            return;
        };

        match header.stream {
            StreamKind::Control => {
                self.control
                    .handle_datagram(datagram, Instant::now(), control_events)
                    .ok();
            }
            StreamKind::Video => {
                self.video.push(datagram, Instant::now(), events).ok();
            }
            StreamKind::Audio => {
                let packets = complete(&mut self.audio, datagram);
                self.audio_packets.extend(packets);
            }
            StreamKind::Cursor => {
                let messages = complete(&mut self.cursor, datagram);
                self.cursor_messages.extend(
                    messages
                        .iter()
                        .map(|frame| CursorMessage::decode(&frame.data).unwrap()),
                );
            }
            StreamKind::Fec
                if FecHeader::parse(payload)
                    .is_ok_and(|(fec, _)| fec.protects == StreamKind::Video) =>
            {
                self.video.push(datagram, Instant::now(), events).ok();
            }
            _ => {}
        }
    }

    fn has_hello(&self) -> bool {
        self.messages
            .iter()
            .any(|message| matches!(message, ControlMessage::Hello { .. }))
    }

    fn has(&self, wanted: ControlMessage) -> bool {
        self.messages.contains(&wanted)
    }
}

fn complete(reassembler: &mut Reassembler, datagram: &[u8]) -> Vec<ReassembledFrame> {
    let mut events = Vec::new();
    reassembler.push(datagram, Instant::now(), &mut events).ok();

    events
        .into_iter()
        .filter_map(|event| match event {
            ReassemblyEvent::Frame(frame) => Some(frame),
            _ => None,
        })
        .collect()
}

struct Fixture {
    dir: TempDir,
    host: Arc<HostSession>,
    address: SocketAddr,
}

impl Fixture {
    fn new(config: Config) -> Self {
        Self::with_session(config, HostSession::without_audio)
    }

    fn with_session(config: Config, configure: impl FnOnce(HostSession) -> HostSession) -> Self {
        let dir = TempDir::new().unwrap();
        let store = PeerStore::open(dir.path().join("paired-clients")).unwrap();
        let host = Arc::new(configure(HostSession::bind(config, store).unwrap()));
        let address = host.local_addr().unwrap();

        Self { dir, host, address }
    }

    fn serve(
        &self,
        pin: Option<Pin>,
        feed: Arc<CaptureFeed>,
        factory: PipelineFactory,
    ) -> thread::JoinHandle<Result<(Admission, HostStream), HostError>> {
        let host = Arc::clone(&self.host);
        thread::spawn(move || {
            let client = host.accept(pin)?;
            let stream = host.serve_with(&client, &feed, factory)?;
            Ok((client.admission(), stream))
        })
    }
}

fn wait_for_end(stream: HostStream) -> Result<(), HostError> {
    let deadline = Instant::now() + WAIT;
    while !stream.is_finished() && Instant::now() < deadline {
        thread::sleep(POLL);
    }
    stream.wait()
}

#[test]
fn a_paired_client_receives_numbered_frames_that_start_with_a_keyframe() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(40), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (admission, stream) = serving.join().unwrap().unwrap();
    let complete = client.pump_until(|client| {
        client.frames.len() >= 4 && client.has_hello() && stream.stats().frames_sent >= 4
    });
    let stats = stream.stats();
    let later_keyframes = client.frames[1..]
        .iter()
        .filter(|frame| frame.keyframe)
        .count() as u64;

    assert!(complete);
    assert_eq!(admission, Admission::Paired);
    assert_eq!(fixture.host.peers().read().len(), 1);
    assert_eq!(
        fixture.host.peers().read().iter().next().unwrap().name(),
        CLIENT_NAME
    );
    assert!(client.frames[0].keyframe);
    assert!(later_keyframes <= stats.frames_dropped);
    assert!(
        client
            .frames
            .iter()
            .zip(0..)
            .all(|(frame, number)| frame.number == number)
    );
    assert!(stats.frames_sent >= 4);
    assert!(stats.keyframes_sent <= 1 + stats.frames_dropped);
    stream.stop().unwrap();
}

#[test]
fn the_stream_format_is_announced_before_the_first_frame_and_again_on_a_resize() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), resizing_feed(6, 3), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let complete =
        client.pump_until(|client| client.frames.len() >= 6 && client.formats.len() >= 2);

    assert!(complete);
    let [(before_first, first), (_, second)] = client.formats[..] else {
        panic!("expected two announcements, got {:?}", client.formats);
    };
    assert_eq!(before_first, 0);
    assert_eq!(first.codec, Codec::Av1);
    assert_eq!(first.resolution, FRAME_SIZE);
    assert_eq!(second.resolution, RESIZED);
    assert!(second.is_newer_than(&first));
    assert!(client.frames[3].keyframe);
    stream.stop().unwrap();
}

#[test]
fn nothing_streams_before_the_client_says_hello() {
    let fixture = Fixture::new(host_config());
    stream_started().store(true, Ordering::Release);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(3), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    let early = client.pump(Duration::from_millis(300), |client| {
        !client.frames.is_empty()
    });
    client.hello();
    let streamed = client.pump_until(|client| !client.frames.is_empty());

    assert!(!early);
    assert!(streamed);
    assert!(client.frames[0].keyframe);
    assert_eq!(client.frames[0].fill, 3);
    stream.stop().unwrap();
}

#[test]
fn a_keyframe_request_on_a_static_screen_repeats_the_last_frame_as_a_keyframe() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(3), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    client.pump_until(|client| client.frames.len() >= 3);
    client.send(ControlMessage::RequestKeyframe { frame: 3 });
    let repeated = client.pump_until(|client| client.frames.len() >= 4);
    let stats = stream.stats();

    assert!(repeated);
    assert_eq!(
        client.frames[3],
        ReceivedFrame {
            number: 3,
            keyframe: true,
            fill: client.frames[2].fill,
        }
    );
    assert_eq!(stats.frames_repeated, 1);
    assert_eq!(stats.keyframe_requests, 1);
    stream.stop().unwrap();
}

#[test]
fn a_request_older_than_the_last_keyframe_is_ignored() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    client.pump_until(|client| !client.frames.is_empty());
    client.send(ControlMessage::RequestKeyframe { frame: 0 });
    client.pump_until(|client| client.frames.len() >= 2);
    client.send(ControlMessage::RequestKeyframe { frame: 0 });
    let settled = client.pump_until(|_| stream.stats().keyframe_requests >= 2);
    client.pump(Duration::from_millis(100), |_| false);
    let stats = stream.stats();

    assert!(settled);
    assert!(client.frames[1].keyframe);
    assert_eq!(stats.frames_repeated, 1);
    assert_eq!(stats.keyframe_requests_ignored, 1);
    assert_eq!(client.frames.len(), 2);
    stream.stop().unwrap();
}

#[test]
fn a_bitrate_hint_reaches_the_encoder() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    client.send(ControlMessage::BitrateHint { kbps: 12_345 });
    let applied = client.pump_until(|_| bitrate.load(Ordering::Relaxed) == 12_345);

    assert!(applied);
    assert_eq!(stream.stats().bitrate_changes, 1);
    stream.stop().unwrap();
}

#[test]
fn a_client_goodbye_ends_the_session_cleanly() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(2), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    client.pump_until(|client| !client.frames.is_empty());
    client.send(ControlMessage::Goodbye);

    assert!(wait_for_end(stream).is_ok());
}

#[test]
fn stopping_the_host_says_goodbye_and_keeps_the_last_frame_for_the_next_client() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let feed = feed(2);
    let serving = fixture.serve(Some(pin()), Arc::clone(&feed), fake_factory(&bitrate));

    let (mut client, record) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    client.pump_until(|client| client.frames.len() >= 2);
    stream.stop().unwrap();
    let farewell = client.pump_until(|client| client.has(ControlMessage::Goodbye));

    let serving = fixture.serve(None, feed, fake_factory(&bitrate));
    let mut returning = TestClient::reconnect(fixture.address, &record);
    returning.hello();
    let (admission, stream) = serving.join().unwrap().unwrap();
    let resumed = returning.pump_until(|client| !client.frames.is_empty());

    assert!(farewell);
    assert_eq!(admission, Admission::Reconnected);
    assert!(resumed);
    assert_eq!(
        returning.frames[0],
        ReceivedFrame {
            number: 0,
            keyframe: true,
            fill: 2,
        }
    );
    stream.stop().unwrap();
}

#[test]
fn a_silent_client_times_out() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    client.pump_until(|client| !client.frames.is_empty());
    drop(client);

    assert!(matches!(
        wait_for_end(stream),
        Err(HostError::ClientSilent(_))
    ));
}

#[test]
fn keepalives_hold_a_static_session_open() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let alive = client.pump_until(|client| {
        client
            .messages
            .iter()
            .filter(|message| **message == ControlMessage::Keepalive)
            .count()
            >= 12
    });

    assert!(alive);
    assert!(!stream.is_finished());
    stream.stop().unwrap();
}

#[test]
fn a_client_that_never_says_hello_times_out() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (_client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();

    assert!(matches!(
        wait_for_end(stream),
        Err(HostError::HandshakeTimeout(_))
    ));
}

#[test]
fn an_unknown_client_cannot_reconnect() {
    let mut config = host_config();
    config.network.pairing_timeout_ms = 1_000;
    let fixture = Fixture::new(config);
    let host = Arc::clone(&fixture.host);
    let accepting = thread::spawn(move || host.accept(None).map(|client| client.admission()));
    let stranger =
        PeerRecord::new("stranger", rusplay_net::PeerSecret::from_bytes([7; 32])).unwrap();

    let client = TestClient::bind(fixture.address);
    let result = reconnect_as_client(
        &client.transport,
        fixture.address,
        &stranger,
        ReconnectTiming::from(&network()),
    );

    assert!(result.is_err());
    assert!(matches!(
        accepting.join().unwrap(),
        Err(HostError::AcceptTimeout(_))
    ));
}

#[test]
fn a_second_client_is_told_the_host_is_busy_while_a_session_runs() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(400), fake_factory(&bitrate));

    let (mut client, record) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let started = client.pump_until(TestClient::has_hello);

    let second = TestClient::bind(fixture.address);
    let pairing = pair_as_client(
        &second.transport,
        fixture.address,
        &pin(),
        ClientName::EMPTY,
        PairingTiming::from(&network()),
    );
    let reconnecting = reconnect_as_client(
        &second.transport,
        fixture.address,
        &record,
        ReconnectTiming::from(&network()),
    );

    assert!(started);
    assert!(matches!(
        pairing,
        Err(NetError::PairingRejected {
            reason: PairingRejection::Busy
        })
    ));
    assert!(matches!(
        reconnecting,
        Err(NetError::ReconnectRejected {
            reason: ReconnectRejection::Busy
        })
    ));
    assert!(client.pump_until(|client| client.frames.len() >= 2));
    stream.stop().unwrap();
}

#[test]
fn a_peer_forgotten_in_the_store_file_cannot_reconnect_to_a_running_host() {
    let mut config = host_config();
    config.network.pairing_timeout_ms = 1_000;
    let fixture = Fixture::new(config);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(40), fake_factory(&bitrate));
    let (mut client, record) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    stream.stop().unwrap();

    let mut external = PeerStore::open(fixture.dir.path().join("paired-clients")).unwrap();
    external.forget(&record.id()).unwrap();
    let host = Arc::clone(&fixture.host);
    let accepting = thread::spawn(move || host.accept(None).map(|client| client.admission()));

    let returning = TestClient::bind(fixture.address);
    let result = reconnect_as_client(
        &returning.transport,
        fixture.address,
        &record,
        ReconnectTiming::from(&network()),
    );

    assert!(matches!(
        result,
        Err(NetError::ReconnectRejected {
            reason: ReconnectRejection::UnknownPeer
        })
    ));
    assert!(fixture.host.peers().read().is_empty());
    assert!(matches!(
        accepting.join().unwrap(),
        Err(HostError::AcceptTimeout(_))
    ));
}

#[test]
fn accept_gives_up_after_the_pairing_timeout() {
    let mut config = host_config();
    config.network.pairing_timeout_ms = 50;
    let fixture = Fixture::new(config);

    assert!(matches!(
        fixture.host.accept(Some(pin())),
        Err(HostError::AcceptTimeout(_))
    ));
}
