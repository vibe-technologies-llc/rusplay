use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_config::NetworkConfig;
use rusplay_core::{Codec, EncodedFrame, FormatSet, Timestamp};
use rusplay_input::RumbleCommand;
use rusplay_net::{
    ControlChannel, ControlConfig, ControlEvent, NetError, ReassembledFrame, Reassembler,
    ReassemblyEvent, ReassemblyLimits, RecvBatch, Transport,
};
use rusplay_protocol::{
    ControlMessage, CursorMessage, FecHeader, PacketHeader, ProtocolVersion, StreamDescription,
    StreamKind,
};

use crate::{
    ClientError, LatestCursor,
    audio::EncodedAudio,
    bitrate::{BitrateController, BitrateSettings, HintOutbox},
    stats::Counters,
};

const RECV_BATCH_DATAGRAMS: usize = 64;
const IDLE_WAKE: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(crate) struct EncodedVideo {
    pub frame: EncodedFrame,
    pub number: u32,
    pub stream: StreamDescription,
    pub received_at: Instant,
}

#[derive(Debug)]
pub(crate) enum DecodeInput {
    Open(Codec),
    Video(EncodedVideo),
}

pub(crate) type SharedFormat = Arc<Mutex<Option<StreamDescription>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feedback {
    DecodeFailed { frame: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Ended,
}

#[derive(Debug, Clone, Copy)]
struct Timing {
    peer_timeout: Duration,
    keyframe_interval: Duration,
}

#[derive(Debug, Default)]
struct Resync {
    awaiting_keyframe: bool,
    missing_frame: u32,
    last_request: Option<Instant>,
    last_keyframe: Option<u32>,
    last_frame: Option<u32>,
}

impl Resync {
    fn predates_last_keyframe(&self, frame: u32) -> bool {
        self.last_keyframe
            .is_some_and(|keyframe| keyframe.wrapping_sub(frame).cast_signed() >= 0)
    }

    fn retry_due(&self, now: Instant, interval: Duration) -> bool {
        self.awaiting_keyframe
            && self
                .last_request
                .is_none_or(|last| now.saturating_duration_since(last) >= interval)
    }

    fn next_retry(&self, interval: Duration) -> Option<Instant> {
        self.awaiting_keyframe
            .then_some(self.last_request)
            .flatten()
            .map(|last| last + interval)
    }
}

#[derive(Debug)]
struct Adaptation {
    controller: BitrateController,
    outbox: HintOutbox,
}

#[derive(Debug, Default)]
struct Pending {
    video: Vec<ReassemblyEvent>,
    audio: Vec<ReassemblyEvent>,
    cursor: Vec<ReassemblyEvent>,
    control: Vec<ControlEvent>,
}

#[derive(Debug)]
struct AudioIngress {
    reassembler: Reassembler,
    queue: SyncSender<EncodedAudio>,
}

#[derive(Debug)]
pub(crate) struct Links {
    pub decode_queue: SyncSender<DecodeInput>,
    pub audio_queue: Option<SyncSender<EncodedAudio>>,
    pub rumble: Option<Sender<RumbleCommand>>,
    pub cursor: LatestCursor,
    pub format: SharedFormat,
    pub feedback: Receiver<Feedback>,
    pub handshake: Sender<()>,
    pub stop: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
}

#[derive(Debug)]
pub(crate) struct ReceiveStage {
    transport: Arc<Transport>,
    host: SocketAddr,
    decodable: FormatSet,
    stream: Option<StreamDescription>,
    timing: Timing,
    control: ControlChannel,
    video: Reassembler,
    audio: Option<AudioIngress>,
    cursor: Reassembler,
    links: Links,
    handshake_done: bool,
    resync: Resync,
    adaptation: Option<Adaptation>,
    started: Instant,
    last_heard: Instant,
}

impl ReceiveStage {
    pub(crate) fn new(
        transport: Arc<Transport>,
        host: SocketAddr,
        decodable: FormatSet,
        network: &NetworkConfig,
        bitrate: Option<BitrateSettings>,
        mut links: Links,
    ) -> Self {
        let now = Instant::now();
        let adaptation = bitrate.map(|settings| {
            links.counters.set_bitrate_target(settings.ceiling_kbps);
            Adaptation {
                controller: BitrateController::new(settings, now),
                outbox: HintOutbox::default(),
            }
        });
        let limits = ReassemblyLimits::from(network);
        let audio = links.audio_queue.take().map(|queue| AudioIngress {
            reassembler: Reassembler::new(StreamKind::Audio, limits),
            queue,
        });

        Self {
            transport,
            host,
            decodable,
            stream: None,
            timing: Timing {
                peer_timeout: Duration::from_millis(u64::from(network.peer_timeout_ms)),
                keyframe_interval: Duration::from_millis(u64::from(
                    network.keyframe_request_interval_ms,
                )),
            },
            control: ControlChannel::new(ControlConfig::from(network)),
            video: Reassembler::new(StreamKind::Video, limits),
            audio,
            cursor: Reassembler::new(StreamKind::Cursor, limits),
            links,
            handshake_done: false,
            resync: Resync::default(),
            adaptation,
            started: now,
            last_heard: now,
        }
    }

    pub(crate) fn run(mut self) -> Result<(), ClientError> {
        let result = self.serve();
        self.links.stop.store(true, Ordering::Relaxed);

        result
    }

    fn serve(&mut self) -> Result<(), ClientError> {
        let mut batch = RecvBatch::new(RECV_BATCH_DATAGRAMS, usize::from(self.transport.mtu()));
        let mut pending = Pending::default();

        self.control.send(
            ControlMessage::Hello {
                version: ProtocolVersion::CURRENT,
                formats: self.decodable,
            },
            Instant::now(),
        )?;
        self.control.flush(&self.transport, self.host)?;

        loop {
            if self.links.stop.load(Ordering::Relaxed) {
                self.say_goodbye();
                return Ok(());
            }

            let wait = self
                .next_wake(Instant::now())
                .saturating_duration_since(Instant::now());
            self.transport.recv_batch_timeout(&mut batch, wait)?;
            let now = Instant::now();

            for (datagram, from) in batch.iter() {
                if from == self.host {
                    self.last_heard = now;
                    self.route(datagram, now, &mut pending);
                }
            }

            self.poll_timeouts(now, &mut pending);
            self.links.counters.set_rtt(self.control.rtt());

            self.handle_audio(&mut pending.audio);
            self.handle_cursor(&mut pending.cursor);
            let control = self.handle_control(&mut pending.control)?;
            let reassembly = self.handle_reassembly(&mut pending.video, now)?;
            if reassembly == Flow::Ended || control == Flow::Ended {
                return Ok(());
            }

            self.drain_feedback(now)?;
            if self.resync.retry_due(now, self.timing.keyframe_interval) {
                self.send_keyframe_request(now)?;
            }
            self.adapt_bitrate(now)?;

            self.control.flush(&self.transport, self.host)?;
            self.links
                .counters
                .rejected_datagrams
                .set(self.transport.rejected_datagrams());

            if now.saturating_duration_since(self.last_heard) >= self.timing.peer_timeout {
                return Err(ClientError::HostSilent(self.timing.peer_timeout));
            }
        }
    }

    fn poll_timeouts(&mut self, now: Instant, pending: &mut Pending) {
        self.video.poll_timeouts(now, &mut pending.video);
        self.cursor.poll_timeouts(now, &mut pending.cursor);
        if let Some(audio) = &mut self.audio {
            audio.reassembler.poll_timeouts(now, &mut pending.audio);
        }
        self.control.poll(now, &mut pending.control);
    }

    fn next_wake(&self, now: Instant) -> Instant {
        [
            self.control.next_deadline(),
            self.video.next_timeout(),
            self.cursor.next_timeout(),
            self.audio
                .as_ref()
                .and_then(|audio| audio.reassembler.next_timeout()),
            self.resync.next_retry(self.timing.keyframe_interval),
            self.adaptation
                .as_ref()
                .and_then(|adaptation| adaptation.controller.next_deadline(self.control.rtt())),
            Some(self.last_heard + self.timing.peer_timeout),
        ]
        .into_iter()
        .flatten()
        .fold(now + IDLE_WAKE, Instant::min)
    }

    fn route(&mut self, datagram: &[u8], now: Instant, pending: &mut Pending) {
        let accepted = match PacketHeader::parse(datagram) {
            Ok((header, _)) if header.stream == StreamKind::Control => self
                .control
                .handle_datagram(datagram, now, &mut pending.control)
                .is_ok(),
            Ok((header, payload)) if header.stream == StreamKind::Fec => {
                match FecHeader::parse(payload) {
                    Ok((fec, _)) => self.push_media(fec.protects, datagram, now, pending),
                    Err(_) => false,
                }
            }
            Ok((header, _)) => self.push_media(header.stream, datagram, now, pending),
            Err(_) => false,
        };

        if !accepted {
            self.links.counters.malformed_datagrams.increment();
        }
    }

    fn push_media(
        &mut self,
        stream: StreamKind,
        datagram: &[u8],
        now: Instant,
        pending: &mut Pending,
    ) -> bool {
        match stream {
            StreamKind::Video => self.video.push(datagram, now, &mut pending.video).is_ok(),
            StreamKind::Cursor => self.cursor.push(datagram, now, &mut pending.cursor).is_ok(),
            StreamKind::Audio => self.audio.as_mut().is_none_or(|audio| {
                audio
                    .reassembler
                    .push(datagram, now, &mut pending.audio)
                    .is_ok()
            }),
            _ => true,
        }
    }

    fn handle_audio(&mut self, events: &mut Vec<ReassemblyEvent>) {
        for event in events.drain(..) {
            let (ReassemblyEvent::Frame(frame), Some(audio)) = (event, &self.audio) else {
                continue;
            };

            self.links.counters.audio_packets_received.increment();
            let packet = EncodedAudio {
                number: frame.frame,
                data: frame.data,
            };
            if let Err(TrySendError::Full(_)) = audio.queue.try_send(packet) {
                self.links.counters.audio_packets_dropped.increment();
            }
        }
    }

    fn handle_cursor(&mut self, events: &mut Vec<ReassemblyEvent>) {
        for event in events.drain(..) {
            let ReassemblyEvent::Frame(frame) = event else {
                continue;
            };

            match CursorMessage::decode(&frame.data) {
                Ok(message) => {
                    self.links.counters.cursor_updates.increment();
                    self.links.cursor.update(message);
                }
                Err(_) => self.links.counters.malformed_datagrams.increment(),
            }
        }
    }

    fn handle_reassembly(
        &mut self,
        events: &mut Vec<ReassemblyEvent>,
        now: Instant,
    ) -> Result<Flow, ClientError> {
        let counters = Arc::clone(&self.links.counters);

        for event in events.drain(..) {
            match event {
                ReassemblyEvent::Frame(frame) => {
                    counters.frames_received.increment();
                    if frame.recovered_fragments > 0 {
                        counters.frames_recovered.increment();
                    }
                    if self.forward(frame, now)? == Flow::Ended {
                        return Ok(Flow::Ended);
                    }
                }
                ReassemblyEvent::FrameLost { frame } => {
                    counters.frames_lost.increment();
                    self.congested(now);
                    if !self.resync.predates_last_keyframe(frame) {
                        self.request_keyframe(frame, now)?;
                    }
                }
                ReassemblyEvent::PacketsMissing {
                    first_sequence,
                    count,
                } => {
                    counters.packets_missing.add(u64::from(count));
                    self.congested(now);
                    self.report_loss(first_sequence, count, now)?;
                }
            }
        }

        Ok(Flow::Continue)
    }

    fn forward(&mut self, frame: ReassembledFrame, now: Instant) -> Result<Flow, ClientError> {
        let skipped = self
            .resync
            .last_frame
            .map_or(0, |last| frame.frame.wrapping_sub(last).wrapping_sub(1));
        self.resync.last_frame = Some(frame.frame);
        if skipped > 0 && skipped.cast_signed() > 0 {
            self.links.counters.frames_lost.add(u64::from(skipped));
            self.congested(now);
            if !frame.keyframe {
                self.request_keyframe(frame.frame.wrapping_sub(skipped), now)?;
            }
        }

        let Some(stream) = self.stream else {
            self.links.counters.frames_skipped.increment();
            self.request_keyframe(frame.frame, now)?;
            return Ok(Flow::Continue);
        };

        if frame.keyframe {
            self.resync.awaiting_keyframe = false;
            self.resync.last_keyframe = Some(frame.frame);
        } else if self.resync.awaiting_keyframe {
            self.links.counters.frames_skipped.increment();
            return Ok(Flow::Continue);
        }

        let number = frame.frame;
        let video = EncodedVideo {
            frame: EncodedFrame {
                codec: stream.codec,
                timestamp: timestamp_since(self.started, now),
                keyframe: frame.keyframe,
                data: frame.data,
            },
            number,
            stream,
            received_at: now,
        };

        match self.links.decode_queue.try_send(DecodeInput::Video(video)) {
            Ok(()) => Ok(Flow::Continue),
            Err(TrySendError::Full(_)) => {
                self.links.counters.frames_dropped.increment();
                self.congested(now);
                self.request_keyframe(number, now)?;
                Ok(Flow::Continue)
            }
            Err(TrySendError::Disconnected(_)) => Ok(Flow::Ended),
        }
    }

    fn handle_control(&mut self, events: &mut Vec<ControlEvent>) -> Result<Flow, ClientError> {
        let mut flow = Flow::Continue;

        for event in events.drain(..) {
            match event {
                ControlEvent::Received(ControlMessage::Hello { version, formats }) => {
                    if version != ProtocolVersion::CURRENT {
                        return Err(ClientError::VersionMismatch {
                            ours: ProtocolVersion::CURRENT.0,
                            theirs: version.0,
                        });
                    }
                    if self.decodable.intersection(formats).is_empty() {
                        return Err(ClientError::NoCommonFormat {
                            decodable: self.decodable,
                            encodable: formats,
                        });
                    }
                    if !self.handshake_done {
                        self.handshake_done = true;
                        self.links.handshake.send(()).ok();
                    }
                }
                ControlEvent::Received(ControlMessage::StreamFormat(description)) => {
                    self.describe(description);
                }
                ControlEvent::Received(ControlMessage::Goodbye) => flow = Flow::Ended,
                ControlEvent::Received(ControlMessage::Rumble {
                    slot,
                    strong,
                    weak,
                    delay_ms,
                    duration_ms,
                }) => {
                    if let Some(rumble) = &self.links.rumble {
                        rumble
                            .send(RumbleCommand {
                                slot,
                                strong,
                                weak,
                                delay_ms,
                                duration_ms,
                            })
                            .ok();
                    }
                }
                ControlEvent::Undeliverable {
                    message: ControlMessage::Hello { .. },
                    ..
                } if !self.handshake_done => return Err(ClientError::HelloUndeliverable),
                ControlEvent::Undeliverable { id, .. } => {
                    self.links.counters.control_undeliverable.increment();
                    if let Some(adaptation) = &mut self.adaptation {
                        let current = adaptation.controller.target_kbps();
                        adaptation.outbox.undeliverable(id, current);
                    }
                }
                ControlEvent::Acked { id } => {
                    if let Some(adaptation) = &mut self.adaptation {
                        adaptation.outbox.acked(id);
                    }
                }
                ControlEvent::Received(_) => {}
            }
        }

        Ok(flow)
    }

    fn describe(&mut self, description: StreamDescription) {
        if self
            .stream
            .is_some_and(|current| !description.is_newer_than(&current))
        {
            return;
        }

        let reopens = self
            .stream
            .is_none_or(|current| current.codec != description.codec);

        self.stream = Some(description);
        if reopens {
            self.links
                .decode_queue
                .try_send(DecodeInput::Open(description.codec))
                .ok();
        }
        *self.links.format.lock() = Some(description);
        self.links.counters.stream_formats_received.increment();
    }

    fn drain_feedback(&mut self, now: Instant) -> Result<(), ClientError> {
        while let Ok(feedback) = self.links.feedback.try_recv() {
            match feedback {
                Feedback::DecodeFailed { frame } => {
                    self.congested(now);
                    self.request_keyframe(frame, now)?;
                }
            }
        }

        Ok(())
    }

    fn congested(&mut self, now: Instant) {
        let rtt = self.control.rtt();
        if let Some(adaptation) = &mut self.adaptation
            && let Some(kbps) = adaptation.controller.congestion(now, rtt)
        {
            adaptation.outbox.queue(kbps);
        }
    }

    fn adapt_bitrate(&mut self, now: Instant) -> Result<(), ClientError> {
        let rtt = self.control.rtt();
        let Some(adaptation) = &mut self.adaptation else {
            return Ok(());
        };

        if let Some(kbps) = adaptation.controller.poll(now, rtt) {
            adaptation.outbox.queue(kbps);
        }
        self.links
            .counters
            .set_bitrate_target(adaptation.controller.target_kbps());

        let Some(kbps) = adaptation.outbox.ready() else {
            return Ok(());
        };
        match self.control.send(ControlMessage::BitrateHint { kbps }, now) {
            Ok(id) => {
                adaptation.outbox.sent(id);
                self.links.counters.bitrate_hints_sent.increment();
                Ok(())
            }
            Err(NetError::ControlWindowFull { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn request_keyframe(&mut self, frame: u32, now: Instant) -> Result<(), ClientError> {
        let already_waiting = self.resync.awaiting_keyframe;
        self.resync.awaiting_keyframe = true;
        self.resync.missing_frame = frame;

        if !already_waiting || self.resync.retry_due(now, self.timing.keyframe_interval) {
            self.send_keyframe_request(now)?;
        }

        Ok(())
    }

    fn send_keyframe_request(&mut self, now: Instant) -> Result<(), ClientError> {
        let request = ControlMessage::RequestKeyframe {
            frame: self.resync.missing_frame,
        };

        match self.control.send(request, now) {
            Ok(_) => {
                self.resync.last_request = Some(now);
                self.links.counters.keyframe_requests_sent.increment();
                Ok(())
            }
            Err(NetError::ControlWindowFull { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn report_loss(
        &mut self,
        first_sequence: u32,
        count: u16,
        now: Instant,
    ) -> Result<(), ClientError> {
        let report = ControlMessage::ReportLoss {
            first_sequence,
            count,
        };

        match self.control.send(report, now) {
            Ok(_) => {
                self.links.counters.loss_reports_sent.increment();
                Ok(())
            }
            Err(NetError::ControlWindowFull { .. }) => {
                self.links.counters.loss_reports_dropped.increment();
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn say_goodbye(&mut self) {
        if self
            .control
            .send(ControlMessage::Goodbye, Instant::now())
            .is_ok()
        {
            self.control.flush(&self.transport, self.host).ok();
        }
    }
}

fn timestamp_since(started: Instant, now: Instant) -> Timestamp {
    let micros = now.saturating_duration_since(started).as_micros();

    Timestamp::from_micros(u64::try_from(micros).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        sync::mpsc,
        thread::{self, JoinHandle},
    };

    use rusplay_core::{Codec, CodecFormat, ColorSpace, PixelFormat, Resolution};
    use rusplay_net::{FecScheme, Packetizer, TransportOptions};
    use rusplay_protocol::{ControlPacket, CursorImage};

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);
    const AUDIO_QUEUE: usize = 16;

    struct Rig {
        host: Transport,
        client: SocketAddr,
        audio: Option<mpsc::Receiver<EncodedAudio>>,
        cursor: LatestCursor,
        rumble: mpsc::Receiver<RumbleCommand>,
        counters: Arc<Counters>,
        stop: Arc<AtomicBool>,
        stage: Option<JoinHandle<Result<(), ClientError>>>,
        format: SharedFormat,
        decode: Receiver<DecodeInput>,
        _feedback: Sender<Feedback>,
    }

    impl Rig {
        fn start(audio_capacity: Option<usize>) -> Self {
            let network = NetworkConfig::default();
            let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
            let host = Transport::bind(loopback, TransportOptions::from(&network)).unwrap();
            let transport = Transport::bind(loopback, TransportOptions::from(&network)).unwrap();
            let client = transport.local_addr().unwrap();
            let host_address = host.local_addr().unwrap();

            let (decode_queue, decode) = mpsc::sync_channel(4);
            let (feedback_sender, feedback) = mpsc::channel();
            let (handshake, _handshake_done) = mpsc::channel();
            let (audio_queue, audio) = match audio_capacity {
                Some(capacity) => {
                    let (sender, receiver) = mpsc::sync_channel(capacity);
                    (Some(sender), Some(receiver))
                }
                None => (None, None),
            };
            let cursor = LatestCursor::new();
            let (rumble_sender, rumble) = mpsc::channel();
            let counters = Arc::new(Counters::default());
            let stop = Arc::new(AtomicBool::new(false));
            let format = Arc::new(Mutex::new(None));

            let links = Links {
                decode_queue,
                audio_queue,
                rumble: Some(rumble_sender),
                cursor: cursor.clone(),
                format: Arc::clone(&format),
                feedback,
                handshake,
                stop: Arc::clone(&stop),
                counters: Arc::clone(&counters),
            };
            let bitrate = BitrateSettings::from_config(&network, 80_000);
            let stage = ReceiveStage::new(
                Arc::new(transport),
                host_address,
                FormatSet::ALL,
                &network,
                bitrate,
                links,
            );
            let stage = thread::spawn(move || stage.run());

            Self {
                host,
                client,
                audio,
                cursor,
                rumble,
                counters,
                stop,
                stage: Some(stage),
                format,
                decode,
                _feedback: feedback_sender,
            }
        }

        fn packetizer(&self, stream: StreamKind, scheme: FecScheme) -> Packetizer {
            Packetizer::new(stream, self.host.max_datagram_len(&self.client), scheme).unwrap()
        }

        fn send(&self, datagrams: &[Vec<u8>]) {
            for datagram in datagrams {
                self.host.send_to(datagram, self.client).unwrap();
            }
        }

        fn describe(&self, id: u32, description: StreamDescription) {
            let packet = ControlPacket::Message {
                id,
                message: ControlMessage::StreamFormat(description),
            };
            self.send(&[packet.to_datagram().unwrap()]);
        }

        fn next_input(&self) -> DecodeInput {
            self.decode
                .recv_timeout(WAIT)
                .expect("an input reaches the decode queue")
        }

        fn next_video(&self) -> EncodedVideo {
            loop {
                if let DecodeInput::Video(video) = self.next_input() {
                    return video;
                }
            }
        }

        fn next_audio(&self) -> EncodedAudio {
            self.audio
                .as_ref()
                .unwrap()
                .recv_timeout(WAIT)
                .expect("an audio packet reaches the queue")
        }

        fn next_control_message(&self) -> Option<ControlMessage> {
            let deadline = Instant::now() + WAIT;
            let mut buf = vec![0; usize::from(self.host.mtu())];
            while Instant::now() < deadline {
                let Some((len, _)) = self
                    .host
                    .recv_from_timeout(&mut buf, Duration::from_millis(10))
                    .unwrap()
                else {
                    continue;
                };
                if let Ok(ControlPacket::Message { message, .. }) = ControlPacket::read(&buf[..len])
                {
                    return Some(message);
                }
            }
            None
        }

        fn wait_until(&self, condition: impl Fn(&Self) -> bool) -> bool {
            let deadline = Instant::now() + WAIT;
            while Instant::now() < deadline {
                if condition(self) {
                    return true;
                }
                thread::sleep(Duration::from_millis(1));
            }
            false
        }

        fn finish(mut self) -> Result<(), ClientError> {
            self.stop.store(true, Ordering::Relaxed);
            self.stage.take().unwrap().join().unwrap()
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(stage) = self.stage.take() {
                stage.join().ok();
            }
        }
    }

    fn payload(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|index| u8::try_from(index % 251).unwrap().wrapping_add(seed))
            .collect()
    }

    fn cursor_bytes(message: &CursorMessage) -> Vec<u8> {
        let mut bytes = Vec::new();
        message.encode(&mut bytes).unwrap();
        bytes
    }

    fn cursor_image(side: u16, marker: u8) -> CursorImage {
        CursorImage {
            width: side,
            height: side,
            hotspot_x: 1,
            hotspot_y: 2,
            rgba: vec![marker; CursorImage::pixel_bytes(side, side)],
        }
    }

    fn description(generation: u32, width: u32) -> StreamDescription {
        StreamDescription {
            generation,
            codec: Codec::Hevc,
            resolution: Resolution::new(width, 720),
            format: PixelFormat::default(),
            color: ColorSpace::default(),
            configured: CodecFormat::new(Codec::Hevc, PixelFormat::default()),
        }
    }

    #[test]
    fn video_waits_for_the_stream_description_and_carries_the_newest() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.send(&packetizer.packetize(0, true, &payload(64, 0)).unwrap());
        assert!(rig.wait_until(|rig| rig.counters.snapshot().frames_skipped == 1));
        rig.describe(0, description(7, 1280));
        assert!(rig.wait_until(|rig| rig.counters.snapshot().stream_formats_received == 1));
        rig.send(&packetizer.packetize(1, true, &payload(64, 1)).unwrap());
        let first = rig.next_video();
        rig.describe(1, description(6, 640));
        rig.describe(2, description(8, 1920));
        assert!(rig.wait_until(|rig| rig.counters.snapshot().stream_formats_received == 2));
        rig.send(&packetizer.packetize(2, false, &payload(64, 2)).unwrap());
        let second = rig.next_video();

        assert_eq!(first.number, 1);
        assert_eq!(first.stream, description(7, 1280));
        assert_eq!(first.frame.codec, Codec::Hevc);
        assert_eq!(second.stream, description(8, 1920));
        assert_eq!(*rig.format.lock(), Some(description(8, 1920)));
        assert!(rig.decode.try_recv().is_err());
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn lost_video_sends_a_lower_bitrate_hint() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        assert_eq!(rig.counters.snapshot().bitrate_target_kbps, Some(80_000));
        rig.describe(0, description(1, 1280));
        rig.send(&packetizer.packetize(0, true, &payload(64, 0)).unwrap());
        rig.next_video();
        packetizer.packetize(1, false, &payload(64, 1)).unwrap();
        rig.send(&packetizer.packetize(2, false, &payload(64, 2)).unwrap());

        let hint = std::iter::from_fn(|| rig.next_control_message())
            .find(|message| matches!(message, ControlMessage::BitrateHint { .. }));
        let stats = rig.counters.snapshot();

        assert_eq!(hint, Some(ControlMessage::BitrateHint { kbps: 60_000 }));
        assert_eq!(stats.bitrate_hints_sent, 1);
        assert_eq!(stats.bitrate_target_kbps, Some(60_000));
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_new_codec_in_a_stream_description_queues_a_decoder_open_ahead_of_video() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);
        let av1 = StreamDescription {
            codec: Codec::Av1,
            ..description(9, 1920)
        };

        rig.describe(0, description(1, 1280));
        assert!(matches!(rig.next_input(), DecodeInput::Open(Codec::Hevc)));

        rig.describe(1, description(2, 1920));
        rig.describe(2, av1);
        assert!(matches!(rig.next_input(), DecodeInput::Open(Codec::Av1)));

        rig.send(&packetizer.packetize(0, true, &payload(64, 0)).unwrap());
        assert!(matches!(rig.next_input(), DecodeInput::Video(_)));
        assert!(rig.decode.try_recv().is_err());
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn audio_packets_reach_the_queue_in_order() {
        let rig = Rig::start(Some(AUDIO_QUEUE));
        let mut packetizer = rig.packetizer(StreamKind::Audio, FecScheme::None);

        for number in 0..5_u32 {
            let datagrams = packetizer
                .packetize(number, false, &payload(160, u8::try_from(number).unwrap()))
                .unwrap();
            rig.send(&datagrams);
        }

        for number in 0..5_u32 {
            let packet = rig.next_audio();
            assert_eq!(packet.number, number);
            assert_eq!(packet.data, payload(160, u8::try_from(number).unwrap()));
        }
        assert_eq!(rig.counters.snapshot().audio_packets_received, 5);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_missing_audio_frame_shows_as_a_gap_in_the_numbers() {
        let rig = Rig::start(Some(AUDIO_QUEUE));
        let mut packetizer = rig.packetizer(StreamKind::Audio, FecScheme::None);

        for number in [0_u32, 1, 3, 4] {
            rig.send(
                &packetizer
                    .packetize(number, false, &payload(100, 1))
                    .unwrap(),
            );
        }

        let numbers: Vec<u32> = (0..4).map(|_| rig.next_audio().number).collect();
        assert_eq!(numbers, [0, 1, 3, 4]);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_fragmented_audio_packet_with_a_lost_fragment_is_recovered() {
        let rig = Rig::start(Some(AUDIO_QUEUE));
        let scheme = FecScheme::ReedSolomon {
            data_shards: 4,
            parity_shards: 1,
        };
        let mut packetizer = rig.packetizer(StreamKind::Audio, scheme);
        let data = payload(4 * packetizer.shard_len(), 9);

        let datagrams = packetizer.packetize(0, false, &data).unwrap();
        let kept: Vec<Vec<u8>> = datagrams
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != 1)
            .map(|(_, datagram)| datagram.clone())
            .collect();
        rig.send(&kept);

        assert_eq!(rig.next_audio().data, data);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_full_audio_queue_drops_the_newest_packet_and_counts_it() {
        let rig = Rig::start(Some(1));
        let mut packetizer = rig.packetizer(StreamKind::Audio, FecScheme::None);

        for number in 0..4_u32 {
            rig.send(
                &packetizer
                    .packetize(number, false, &payload(64, 0))
                    .unwrap(),
            );
        }

        assert!(rig.wait_until(|rig| rig.counters.snapshot().audio_packets_received == 4));
        assert_eq!(rig.counters.snapshot().audio_packets_dropped, 3);
        assert_eq!(rig.next_audio().number, 0);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn audio_is_ignored_without_an_output() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Audio, FecScheme::None);
        let mut cursor = rig.packetizer(StreamKind::Cursor, FecScheme::None);

        rig.send(&packetizer.packetize(0, false, &payload(64, 0)).unwrap());
        let position = CursorMessage {
            x: 1,
            y: 2,
            space: Resolution::new(1920, 1080),
            visible: true,
            image: None,
        };
        rig.send(
            &cursor
                .packetize(0, false, &cursor_bytes(&position))
                .unwrap(),
        );

        assert!(rig.wait_until(|rig| rig.cursor.updates() == 1));
        let stats = rig.counters.snapshot();
        assert_eq!(stats.audio_packets_received, 0);
        assert_eq!(stats.malformed_datagrams, 0);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn cursor_updates_land_in_the_latest_slot() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Cursor, FecScheme::None);
        let with_image = CursorMessage {
            x: 10,
            y: 20,
            space: Resolution::new(1920, 1080),
            visible: true,
            image: Some(cursor_image(2, 5)),
        };
        let moved = CursorMessage {
            x: 30,
            y: 40,
            space: Resolution::new(1920, 1080),
            visible: false,
            image: None,
        };

        rig.send(
            &packetizer
                .packetize(0, false, &cursor_bytes(&with_image))
                .unwrap(),
        );
        rig.send(
            &packetizer
                .packetize(1, false, &cursor_bytes(&moved))
                .unwrap(),
        );

        assert!(rig.wait_until(|rig| rig.cursor.updates() == 2));
        let latest = rig.cursor.latest().unwrap();
        assert_eq!((latest.x, latest.y, latest.visible), (30, 40, false));
        assert_eq!(latest.image.unwrap().rgba, vec![5; 16]);
        assert_eq!(rig.counters.snapshot().cursor_updates, 2);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_cursor_image_larger_than_a_datagram_is_reassembled() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Cursor, FecScheme::None);
        let message = CursorMessage {
            x: 1,
            y: 1,
            space: Resolution::new(1920, 1080),
            visible: true,
            image: Some(cursor_image(48, 7)),
        };
        let bytes = cursor_bytes(&message);
        assert!(bytes.len() > packetizer.shard_len());

        rig.send(&packetizer.packetize(0, false, &bytes).unwrap());

        assert!(rig.wait_until(|rig| rig.cursor.updates() == 1));
        let image = rig.cursor.latest().unwrap().image.unwrap();
        assert_eq!((image.width, image.height), (48, 48));
        assert_eq!(image.rgba, vec![7; 48 * 48 * 4]);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_malformed_cursor_message_is_counted_and_ignored() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Cursor, FecScheme::None);

        rig.send(&packetizer.packetize(0, false, &[1, 2, 3]).unwrap());

        assert!(rig.wait_until(|rig| rig.counters.snapshot().malformed_datagrams == 1));
        assert_eq!(rig.cursor.latest(), None);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_stale_cursor_frame_does_not_replace_a_newer_one() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Cursor, FecScheme::None);
        let at = |x| CursorMessage {
            x,
            y: 0,
            space: Resolution::new(1920, 1080),
            visible: true,
            image: None,
        };

        rig.send(
            &packetizer
                .packetize(5, false, &cursor_bytes(&at(50)))
                .unwrap(),
        );
        assert!(rig.wait_until(|rig| rig.cursor.updates() == 1));
        rig.send(
            &packetizer
                .packetize(4, false, &cursor_bytes(&at(40)))
                .unwrap(),
        );
        rig.send(
            &packetizer
                .packetize(6, false, &cursor_bytes(&at(60)))
                .unwrap(),
        );

        assert!(rig.wait_until(|rig| rig.cursor.updates() == 2));
        assert_eq!(rig.cursor.latest().unwrap().x, 60);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn rumble_messages_are_forwarded_in_order_and_acknowledged() {
        let rig = Rig::start(None);
        let messages = [
            ControlMessage::Rumble {
                slot: 1,
                strong: 0xFFFF,
                weak: 0x4000,
                delay_ms: 3,
                duration_ms: 250,
            },
            ControlMessage::Rumble {
                slot: 1,
                strong: 0,
                weak: 0,
                delay_ms: 0,
                duration_ms: 0,
            },
        ];

        for (id, message) in messages.into_iter().enumerate() {
            let packet = ControlPacket::Message {
                id: u32::try_from(id).unwrap(),
                message,
            };
            rig.send(&[packet.to_datagram().unwrap()]);
        }

        let commands: Vec<_> = (0..2)
            .map(|_| {
                rig.rumble
                    .recv_timeout(WAIT)
                    .expect("a rumble command arrives")
            })
            .collect();
        assert_eq!(
            commands,
            vec![
                RumbleCommand {
                    slot: 1,
                    strong: 0xFFFF,
                    weak: 0x4000,
                    delay_ms: 3,
                    duration_ms: 250,
                },
                RumbleCommand::stop(1),
            ]
        );
        assert!(rig.finish().is_ok());
    }
}
