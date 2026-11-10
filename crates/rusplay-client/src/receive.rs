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
use rusplay_clipboard::{ClipboardError, ClipboardLink};
use rusplay_config::{LossRecovery, NetworkConfig};
use rusplay_core::{Codec, EncodedFrame, FormatSet, Timestamp};
use rusplay_input::{PadFeedback, RumbleCommand};
use rusplay_net::{
    ControlChannel, ControlConfig, ControlEvent, NetError, ReassembledFrame, Reassembler,
    ReassemblyEvent, ReassemblyLimits, RecvBatch, Transport, proves_peer_liveness,
};
use rusplay_protocol::{
    ClientDisplay, ClipboardChunk, ControlMessage, CursorMessage, FecHeader, PacketHeader,
    ProtocolVersion, StreamDescription, StreamKind, VideoFrameHeader,
};

use crate::{
    ClientError, LatestCursor,
    audio::EncodedAudio,
    bitrate::{BitrateSettings, BitrateSteering},
    clock::ClockSync,
    quality::QualityRequester,
    resize::DisplayAnnouncer,
    stats::Counters,
};

const RECV_BATCH_DATAGRAMS: usize = 64;
const IDLE_WAKE: Duration = Duration::from_millis(10);
const GOODBYE_LINGER_RETRANSMITS: u32 = 4;

#[derive(Debug)]
pub(crate) struct EncodedVideo {
    pub frame: EncodedFrame,
    pub number: u32,
    pub stream: StreamDescription,
    #[cfg_attr(not(test), allow(dead_code))]
    pub recovers_from: Option<u32>,
    pub captured_micros: u64,
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
    Decoded { frame: u32 },
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
    ack_interval: Option<Duration>,
    loss_recovery: LossRecovery,
    goodbye_linger: Duration,
}

#[derive(Debug, Default)]
struct Resync {
    awaiting_keyframe: bool,
    missing_frame: u32,
    last_request: Option<Instant>,
    last_keyframe: Option<u32>,
    last_frame: Option<u32>,
    last_good: Option<u32>,
    recovery_tried: bool,
    keyframe_only: bool,
}

impl Resync {
    fn decoded(&mut self, frame: u32) {
        if self
            .last_good
            .is_none_or(|good| frame.wrapping_sub(good).cast_signed() > 0)
        {
            self.last_good = Some(frame);
        }
        if (self.awaiting_keyframe || self.recovery_tried || self.keyframe_only)
            && frame.wrapping_sub(self.missing_frame).cast_signed() >= 0
        {
            self.awaiting_keyframe = false;
            self.recovery_tried = false;
            self.keyframe_only = false;
        }
    }

    fn can_resume_from(&self, reference: u32) -> bool {
        self.last_good
            .is_some_and(|good| good.wrapping_sub(reference).cast_signed() >= 0)
            && self
                .last_keyframe
                .is_none_or(|keyframe| reference.wrapping_sub(keyframe).cast_signed() >= 0)
    }

    fn predates_last_keyframe(&self, frame: u32) -> bool {
        self.last_keyframe
            .is_some_and(|keyframe| keyframe.wrapping_sub(frame).cast_signed() >= 0)
    }

    fn advance_to(&mut self, frame: u32) -> u32 {
        let Some(last) = self.last_frame else {
            self.last_frame = Some(frame);
            return 0;
        };

        let distance = frame.wrapping_sub(last);
        if distance.cast_signed() <= 0 {
            return 0;
        }

        self.last_frame = Some(frame);
        distance - 1
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

#[derive(Debug, Default)]
struct Acks {
    sent: Option<u32>,
    last_sent: Option<Instant>,
}

impl Acks {
    fn next_due(
        &self,
        now: Instant,
        interval: Option<Duration>,
        last_good: Option<u32>,
    ) -> Option<Instant> {
        let interval = interval?;
        let good = last_good?;
        if self.sent == Some(good) {
            return None;
        }

        Some(self.last_sent.map_or(now, |last| last + interval))
    }
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

fn pad_feedback(message: ControlMessage) -> Option<PadFeedback> {
    match message {
        ControlMessage::Rumble {
            slot,
            strong,
            weak,
            delay_ms,
            duration_ms,
        } => Some(PadFeedback::Rumble(RumbleCommand {
            slot,
            strong,
            weak,
            delay_ms,
            duration_ms,
        })),
        ControlMessage::Lightbar {
            slot,
            red,
            green,
            blue,
        } => Some(PadFeedback::Lightbar {
            slot,
            red,
            green,
            blue,
        }),
        ControlMessage::PlayerLeds { slot, mask } => Some(PadFeedback::PlayerLeds { slot, mask }),
        ControlMessage::TriggerEffect {
            slot,
            right,
            effect,
        } => Some(PadFeedback::TriggerEffect {
            slot,
            right,
            effect,
        }),
        _ => None,
    }
}

pub(crate) type DisplayUpdates = Arc<Mutex<Option<ClientDisplay>>>;

#[derive(Debug)]
pub(crate) struct Links {
    pub decode_queue: SyncSender<DecodeInput>,
    pub audio_queue: Option<SyncSender<EncodedAudio>>,
    pub pad_feedback: Option<Sender<PadFeedback>>,
    pub clipboard: Option<ClipboardLink>,
    pub cursor: LatestCursor,
    pub format: SharedFormat,
    pub feedback: Receiver<Feedback>,
    pub handshake: Sender<()>,
    pub display: ClientDisplay,
    pub display_updates: DisplayUpdates,
    pub display_debounce: Duration,
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
    acks: Acks,
    steering: Option<BitrateSteering>,
    announcer: DisplayAnnouncer,
    quality: QualityRequester,
    clock_sync: ClockSync,
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
        let announcer = DisplayAnnouncer::new(links.display, links.display_debounce);
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
                ack_interval: (network.frame_ack_interval_ms > 0)
                    .then(|| Duration::from_millis(u64::from(network.frame_ack_interval_ms))),
                loss_recovery: network.loss_recovery,
                goodbye_linger: Duration::from_millis(u64::from(network.control_retransmit_ms))
                    * GOODBYE_LINGER_RETRANSMITS,
            },
            control: ControlChannel::new(ControlConfig::from(network)),
            video: Reassembler::new(StreamKind::Video, limits),
            audio,
            cursor: Reassembler::new(StreamKind::Cursor, limits),
            links,
            handshake_done: false,
            resync: Resync::default(),
            acks: Acks::default(),
            steering: bitrate.map(BitrateSteering::new),
            announcer,
            quality: QualityRequester::default(),
            clock_sync: ClockSync::new(network.clock_sync_interval_ms, now),
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
        let announced = self
            .control
            .send(ControlMessage::Display(self.links.display), Instant::now())?;
        self.announcer.sent(self.links.display, announced);
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
                    if proves_peer_liveness(datagram) {
                        self.last_heard = now;
                    }
                    self.route(datagram, now, &mut pending);
                }
            }

            self.poll_timeouts(now, &mut pending);
            self.links.counters.set_rtt(self.control.rtt());

            self.handle_audio(&mut pending.audio);
            self.handle_cursor(&mut pending.cursor);
            let control = self.handle_control(&mut pending.control, now)?;
            let reassembly = self.handle_reassembly(&mut pending.video, now)?;
            if control == Flow::Ended {
                return Ok(());
            }
            if reassembly == Flow::Ended {
                self.say_goodbye();
                return Ok(());
            }

            self.drain_feedback(now)?;
            self.send_clock_ping(now)?;
            if self.resync.retry_due(now, self.timing.keyframe_interval) {
                self.send_refresh_request(now)?;
            }
            self.send_frame_ack(now)?;
            self.adapt_bitrate(now)?;
            self.announce_display(now)?;
            self.request_quality(now)?;
            self.service_clipboard(now);

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
            self.announcer.next_deadline(),
            self.acks
                .next_due(now, self.timing.ack_interval, self.resync.last_good),
            self.steering
                .as_ref()
                .and_then(|steering| steering.next_deadline(self.control.rtt())),
            self.handshake_done
                .then(|| self.clock_sync.next_ping())
                .flatten(),
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
                data: frame.data.into_vec(),
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
                    let unseen = self.resync.advance_to(frame);
                    counters.frames_lost.add(1 + u64::from(unseen));
                    self.congested(now);
                    if !self.resync.predates_last_keyframe(frame) {
                        self.request_refresh(frame, now)?;
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
        let skipped = self.resync.advance_to(frame.frame);
        if skipped > 0 {
            self.links.counters.frames_lost.add(u64::from(skipped));
            self.congested(now);
            if !frame.keyframe {
                self.request_refresh(frame.frame.wrapping_sub(skipped), now)?;
            }
        }

        let Some(stream) = self.stream else {
            self.links.counters.frames_skipped.increment();
            self.request_refresh(frame.frame, now)?;
            return Ok(Flow::Continue);
        };

        let number = frame.frame;
        let Ok((header, _)) = VideoFrameHeader::parse(&frame.data) else {
            self.links.counters.malformed_datagrams.increment();
            self.request_refresh(number, now)?;
            return Ok(Flow::Continue);
        };

        if frame.keyframe {
            self.resync.awaiting_keyframe = false;
            self.resync.keyframe_only = false;
            self.resync.last_keyframe = Some(number);
        } else if self.resync.keyframe_only {
            self.links.counters.frames_skipped.increment();
            return Ok(Flow::Continue);
        } else if self.resync.awaiting_keyframe {
            match header.recovers_from {
                Some(reference) if self.resync.can_resume_from(reference) => {
                    self.resync.awaiting_keyframe = false;
                    self.links.counters.reference_recoveries.increment();
                }
                _ => {
                    self.links.counters.frames_skipped.increment();
                    return Ok(Flow::Continue);
                }
            }
        }

        let captured_micros = header.captured_micros;
        let data = frame.data.skip(VideoFrameHeader::SIZE);

        if let Some(latency) = self
            .links
            .counters
            .clock
            .since_capture(captured_micros, now)
        {
            self.links.counters.capture_to_receive.record(latency);
        }

        let video = EncodedVideo {
            frame: EncodedFrame {
                codec: stream.codec,
                timestamp: Timestamp::from_micros(captured_micros),
                keyframe: frame.keyframe,
                data,
            },
            number,
            stream,
            recovers_from: header.recovers_from,
            captured_micros,
            received_at: now,
        };

        match self.links.decode_queue.try_send(DecodeInput::Video(video)) {
            Ok(()) => Ok(Flow::Continue),
            Err(TrySendError::Full(_)) => {
                self.links.counters.frames_dropped.increment();
                self.congested(now);
                self.request_refresh(number, now)?;
                Ok(Flow::Continue)
            }
            Err(TrySendError::Disconnected(_)) => Ok(Flow::Ended),
        }
    }

    fn handle_control(
        &mut self,
        events: &mut Vec<ControlEvent>,
        now: Instant,
    ) -> Result<Flow, ClientError> {
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
                ControlEvent::Received(ControlMessage::ClockPong {
                    sent_micros,
                    host_micros,
                }) => self.record_clock(sent_micros, host_micros, now),
                ControlEvent::Received(ControlMessage::Goodbye) => flow = Flow::Ended,
                ControlEvent::Received(ControlMessage::BitrateCeiling { kbps }) => {
                    if let Some(steering) = &mut self.steering {
                        steering.host_ceiling(kbps, now);
                    }
                }
                ControlEvent::Received(
                    message @ (ControlMessage::Rumble { .. }
                    | ControlMessage::Lightbar { .. }
                    | ControlMessage::PlayerLeds { .. }
                    | ControlMessage::TriggerEffect { .. }),
                ) => {
                    if let (Some(pad), Some(feedback)) =
                        (&self.links.pad_feedback, pad_feedback(message))
                    {
                        pad.send(feedback).ok();
                    }
                }
                ControlEvent::Undeliverable {
                    message: ControlMessage::Hello { .. },
                    ..
                } if !self.handshake_done => return Err(ClientError::HelloUndeliverable),
                ControlEvent::Undeliverable { id, .. } => {
                    self.links.counters.control_undeliverable.increment();
                    if let Some(steering) = &mut self.steering {
                        steering.undeliverable(id);
                    }
                    self.announcer.undeliverable(id, now);
                    self.quality.undeliverable(id);
                }
                ControlEvent::Acked { id } => {
                    self.announcer.acked(id);
                    self.quality.acked(id);
                    if let Some(steering) = &mut self.steering {
                        steering.acked(id);
                    }
                }
                ControlEvent::Received(ControlMessage::Clipboard(chunk)) => {
                    self.receive_clipboard(&chunk);
                }
                ControlEvent::Received(ControlMessage::QualityReport(report)) => {
                    self.links.counters.quality.report(report, now);
                }
                ControlEvent::Received(ControlMessage::QualityUnavailable { reason }) => {
                    self.links.counters.quality.unavailable(reason);
                }
                ControlEvent::Received(
                    ControlMessage::Keepalive
                    | ControlMessage::RequestKeyframe { .. }
                    | ControlMessage::ReportLoss { .. }
                    | ControlMessage::BitrateHint { .. }
                    | ControlMessage::ClockPing { .. }
                    | ControlMessage::AcknowledgeFrame { .. }
                    | ControlMessage::RequestRecovery { .. }
                    | ControlMessage::Display(_)
                    | ControlMessage::QualityRequest { .. },
                ) => {}
            }
        }

        Ok(flow)
    }

    fn receive_clipboard(&mut self, chunk: &ClipboardChunk) {
        if !self.handshake_done {
            return;
        }
        let Some(link) = &mut self.links.clipboard else {
            return;
        };

        match link.receive(chunk) {
            Ok(()) => {}
            Err(ClipboardError::Protocol(_)) => self.links.counters.malformed_datagrams.increment(),
            Err(_) => self.links.clipboard = None,
        }
    }

    fn service_clipboard(&mut self, now: Instant) {
        if !self.handshake_done {
            return;
        }
        let Some(link) = &mut self.links.clipboard else {
            return;
        };

        let control = &mut self.control;
        let serviced =
            link.service(|chunk| control.send(ControlMessage::Clipboard(chunk), now).is_ok());
        if serviced.is_err() {
            self.links.clipboard = None;
        }
    }

    fn send_clock_ping(&mut self, now: Instant) -> Result<(), ClientError> {
        if !self.handshake_done || !self.clock_sync.ping_due(now) {
            return Ok(());
        }

        self.clock_sync.pinged(now);
        let sent_micros = self.links.counters.clock.local_micros(now).unsigned_abs();
        match self
            .control
            .send(ControlMessage::ClockPing { sent_micros }, now)
        {
            Ok(_) | Err(NetError::ControlWindowFull { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn record_clock(&mut self, sent_micros: u64, host_micros: u64, now: Instant) {
        let clock = &self.links.counters.clock;
        let (Ok(sent), Ok(host)) = (i64::try_from(sent_micros), i64::try_from(host_micros)) else {
            return;
        };

        if let Some(offset) = self.clock_sync.record(sent, host, clock.local_micros(now)) {
            clock.set_offset(offset);
        }
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
            self.resync.last_good = None;
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
                Feedback::Decoded { frame } => self.resync.decoded(frame),
                Feedback::DecodeFailed { frame } => {
                    self.congested(now);
                    self.force_keyframe(frame, now)?;
                }
            }
        }

        Ok(())
    }

    fn congested(&mut self, now: Instant) {
        let rtt = self.control.rtt();
        if let Some(steering) = &mut self.steering {
            steering.congestion(now, rtt);
        }
    }

    fn adapt_bitrate(&mut self, now: Instant) -> Result<(), ClientError> {
        let rtt = self.control.rtt();
        let Some(steering) = &mut self.steering else {
            return Ok(());
        };

        steering.poll(now, rtt);
        self.links
            .counters
            .set_bitrate_target(steering.target_kbps());

        let Some(kbps) = steering.ready() else {
            return Ok(());
        };
        match self.control.send(ControlMessage::BitrateHint { kbps }, now) {
            Ok(id) => {
                steering.sent(id);
                self.links.counters.bitrate_hints_sent.increment();
                Ok(())
            }
            Err(NetError::ControlWindowFull { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn request_quality(&mut self, now: Instant) -> Result<(), ClientError> {
        let wanted = self.links.counters.quality.wanted();
        let Some(enabled) = self.quality.ready(wanted).filter(|_| self.handshake_done) else {
            return Ok(());
        };
        match self
            .control
            .send(ControlMessage::QualityRequest { enabled }, now)
        {
            Ok(id) => {
                self.quality.sent(enabled, id);
                Ok(())
            }
            Err(NetError::ControlWindowFull { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn announce_display(&mut self, now: Instant) -> Result<(), ClientError> {
        if let Some(display) = self.links.display_updates.lock().take() {
            self.announcer.request(display, now);
        }

        let Some(display) = self.announcer.ready(now) else {
            return Ok(());
        };
        match self.control.send(ControlMessage::Display(display), now) {
            Ok(id) => {
                self.announcer.sent(display, id);
                self.links.counters.display_updates_sent.increment();
                Ok(())
            }
            Err(NetError::ControlWindowFull { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn force_keyframe(&mut self, frame: u32, now: Instant) -> Result<(), ClientError> {
        self.resync.recovery_tried = true;
        self.resync.keyframe_only = true;
        self.resync.awaiting_keyframe = false;
        self.request_refresh(frame, now)
    }

    fn request_refresh(&mut self, frame: u32, now: Instant) -> Result<(), ClientError> {
        let already_waiting = self.resync.awaiting_keyframe;
        self.resync.awaiting_keyframe = true;
        self.resync.missing_frame = frame;

        if !already_waiting || self.resync.retry_due(now, self.timing.keyframe_interval) {
            self.send_refresh_request(now)?;
        }

        Ok(())
    }

    fn send_refresh_request(&mut self, now: Instant) -> Result<(), ClientError> {
        let frame = self.resync.missing_frame;
        let recovery = self.resync.last_good.filter(|_| {
            self.timing.loss_recovery == LossRecovery::Reference && !self.resync.recovery_tried
        });
        let request = match recovery {
            Some(last_good) => ControlMessage::RequestRecovery { frame, last_good },
            None => ControlMessage::RequestKeyframe { frame },
        };

        match self.control.send(request, now) {
            Ok(_) => {
                self.resync.last_request = Some(now);
                if recovery.is_some() {
                    self.resync.recovery_tried = true;
                    self.links.counters.recovery_requests_sent.increment();
                } else {
                    self.links.counters.keyframe_requests_sent.increment();
                }
                Ok(())
            }
            Err(NetError::ControlWindowFull { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn send_frame_ack(&mut self, now: Instant) -> Result<(), ClientError> {
        let Some(frame) = self.resync.last_good else {
            return Ok(());
        };
        if self
            .acks
            .next_due(now, self.timing.ack_interval, Some(frame))
            .is_none_or(|due| due > now)
        {
            return Ok(());
        }

        match self
            .control
            .send(ControlMessage::AcknowledgeFrame { frame }, now)
        {
            Ok(_) => {
                self.acks.sent = Some(frame);
                self.acks.last_sent = Some(now);
                self.links.counters.frame_acks_sent.increment();
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
        let deadline = Instant::now() + self.timing.goodbye_linger;
        let mut batch = RecvBatch::new(RECV_BATCH_DATAGRAMS, usize::from(self.transport.mtu()));
        let mut events = Vec::new();
        let mut goodbye = None;

        loop {
            let now = Instant::now();
            if goodbye.is_none() {
                goodbye = self.control.send(ControlMessage::Goodbye, now).ok();
            }
            if self.control.flush(&self.transport, self.host).is_err() {
                return;
            }
            let acknowledged =
                goodbye.is_some_and(|id| events.contains(&ControlEvent::Acked { id }));
            if now >= deadline || acknowledged {
                return;
            }

            let wait = self
                .control
                .next_deadline()
                .unwrap_or(deadline)
                .min(deadline)
                .saturating_duration_since(now);
            if self.transport.recv_batch_timeout(&mut batch, wait).is_err() {
                return;
            }
            for (datagram, from) in batch.iter() {
                if from == self.host
                    && PacketHeader::read(datagram)
                        .is_ok_and(|header| header.stream == StreamKind::Control)
                {
                    self.control
                        .handle_datagram(datagram, Instant::now(), &mut events)
                        .ok();
                }
            }
            self.control.poll(Instant::now(), &mut events);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        sync::mpsc,
        thread::{self, JoinHandle},
    };

    use rusplay_core::{ChromaSiting, Codec, CodecFormat, ColorSpace, PixelFormat, Resolution};
    use rusplay_net::{FecScheme, Packetizer, TransportOptions};
    use rusplay_protocol::{ControlPacket, CursorImage};

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);
    const QUIET: Duration = Duration::from_millis(150);
    const AUDIO_QUEUE: usize = 16;
    const CLIENT_KBPS: u32 = 80_000;

    struct Rig {
        host: Transport,
        client: SocketAddr,
        audio: Option<mpsc::Receiver<EncodedAudio>>,
        cursor: LatestCursor,
        rumble: mpsc::Receiver<PadFeedback>,
        counters: Arc<Counters>,
        stop: Arc<AtomicBool>,
        stage: Option<JoinHandle<Result<(), ClientError>>>,
        format: SharedFormat,
        decode: Receiver<DecodeInput>,
        feedback: Sender<Feedback>,
    }

    impl Rig {
        fn start(audio_capacity: Option<usize>) -> Self {
            Self::start_with(audio_capacity, None)
        }

        fn start_with(audio_capacity: Option<usize>, clipboard: Option<ClipboardLink>) -> Self {
            Self::start_on(NetworkConfig::default(), audio_capacity, clipboard)
        }

        fn start_on(
            network: NetworkConfig,
            audio_capacity: Option<usize>,
            clipboard: Option<ClipboardLink>,
        ) -> Self {
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
                pad_feedback: Some(rumble_sender),
                clipboard,
                cursor: cursor.clone(),
                format: Arc::clone(&format),
                feedback,
                handshake,
                display: crate::announce::default_display(&rusplay_config::Config::default())
                    .unwrap(),
                display_updates: Arc::new(Mutex::new(None)),
                display_debounce: Duration::ZERO,
                stop: Arc::clone(&stop),
                counters: Arc::clone(&counters),
            };
            let bitrate = BitrateSettings::from_config(&network, CLIENT_KBPS);
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
                feedback: feedback_sender,
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
            self.control(id, ControlMessage::StreamFormat(description));
        }

        fn announce_ceiling(&self, id: u32, kbps: u32) {
            self.control(id, ControlMessage::BitrateCeiling { kbps });
        }

        fn control(&self, id: u32, message: ControlMessage) {
            let packet = ControlPacket::Message { id, message };
            self.send(&[packet.to_datagram().unwrap()]);
        }

        fn lose_a_frame(&self, packetizer: &mut Packetizer) {
            self.send(
                &packetizer
                    .packetize(0, true, &video_frame(0, 64, 0))
                    .unwrap(),
            );
            self.next_video();
            packetizer
                .packetize(1, false, &video_frame(0, 64, 1))
                .unwrap();
            self.send(
                &packetizer
                    .packetize(2, false, &video_frame(0, 64, 2))
                    .unwrap(),
            );
        }

        fn loss_noticed(&self) -> bool {
            self.wait_until(|rig| rig.counters.snapshot().frames_lost >= 1)
        }

        fn next_hint(&self) -> Option<ControlMessage> {
            std::iter::from_fn(|| self.next_control_message())
                .find(|message| matches!(message, ControlMessage::BitrateHint { .. }))
        }

        fn control_messages_for(&self, span: Duration) -> Vec<ControlMessage> {
            let deadline = Instant::now() + span;
            let mut buf = vec![0; usize::from(self.host.mtu())];
            let mut messages = Vec::new();
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
                    messages.push(message);
                }
            }
            messages
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

        fn close_decode_queue(&mut self) {
            self.decode = mpsc::sync_channel(1).1;
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

    fn video_frame(captured_micros: u64, len: usize, seed: u8) -> Vec<u8> {
        recovery_frame(captured_micros, None, len, seed)
    }

    fn recovery_frame(
        captured_micros: u64,
        recovers_from: Option<u32>,
        len: usize,
        seed: u8,
    ) -> Vec<u8> {
        let mut frame = Vec::new();
        VideoFrameHeader {
            captured_micros,
            recovers_from,
        }
        .write(&mut frame);
        frame.extend(payload(len, seed));
        frame
    }

    fn decoded_through(rig: &Rig, packetizer: &mut Packetizer, frames: u32) {
        for frame in 0..frames {
            rig.send(
                &packetizer
                    .packetize(frame, frame == 0, &video_frame(0, 64, 0))
                    .unwrap(),
            );
            let video = rig.next_video();
            rig.feedback
                .send(Feedback::Decoded {
                    frame: video.number,
                })
                .unwrap();
        }
    }

    fn next_message_matching(
        rig: &Rig,
        wanted: impl Fn(&ControlMessage) -> bool,
    ) -> Option<ControlMessage> {
        std::iter::from_fn(|| rig.next_control_message()).find(wanted)
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
            siting: ChromaSiting::Left,
        }
    }

    #[test]
    fn an_unacknowledged_goodbye_is_sent_again_until_the_linger_ends() {
        let mut rig = Rig::start(None);
        let mut buf = vec![0; usize::from(rig.host.mtu())];
        let mut goodbyes = 0;

        rig.stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + WAIT;
        while goodbyes < 2 && Instant::now() < deadline {
            let Some((len, _)) = rig
                .host
                .recv_from_timeout(&mut buf, Duration::from_millis(10))
                .unwrap()
            else {
                continue;
            };
            if let Ok(ControlPacket::Message {
                message: ControlMessage::Goodbye,
                ..
            }) = ControlPacket::read(&buf[..len])
            {
                goodbyes += 1;
            }
        }
        let ended = rig.stage.take().unwrap().join().unwrap();

        assert_eq!(goodbyes, 2);
        assert!(ended.is_ok());
    }

    #[test]
    fn a_closed_decode_queue_ends_the_stage_with_a_goodbye() {
        let mut rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        assert!(rig.wait_until(|rig| rig.counters.snapshot().stream_formats_received == 1));
        rig.close_decode_queue();
        rig.send(
            &packetizer
                .packetize(0, true, &video_frame(0, 64, 0))
                .unwrap(),
        );
        let farewell = next_message_matching(&rig, |message| *message == ControlMessage::Goodbye);

        assert_eq!(farewell, Some(ControlMessage::Goodbye));
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn video_waits_for_the_stream_description_and_carries_the_newest() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.send(
            &packetizer
                .packetize(0, true, &video_frame(0, 64, 0))
                .unwrap(),
        );
        assert!(rig.wait_until(|rig| rig.counters.snapshot().frames_skipped == 1));
        rig.describe(0, description(7, 1280));
        assert!(rig.wait_until(|rig| rig.counters.snapshot().stream_formats_received == 1));
        rig.send(
            &packetizer
                .packetize(1, true, &video_frame(1_000, 64, 1))
                .unwrap(),
        );
        let first = rig.next_video();
        rig.describe(1, description(6, 640));
        rig.describe(2, description(8, 1920));
        assert!(rig.wait_until(|rig| rig.counters.snapshot().stream_formats_received == 2));
        rig.send(
            &packetizer
                .packetize(2, false, &video_frame(2_000, 64, 2))
                .unwrap(),
        );
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

        assert_eq!(rig.counters.snapshot().bitrate_target_kbps, None);
        rig.describe(0, description(1, 1280));
        rig.announce_ceiling(1, CLIENT_KBPS);
        assert!(
            rig.wait_until(|rig| rig.counters.snapshot().bitrate_target_kbps == Some(CLIENT_KBPS))
        );
        rig.lose_a_frame(&mut packetizer);

        let hint = rig.next_hint();
        let stats = rig.counters.snapshot();

        assert_eq!(hint, Some(ControlMessage::BitrateHint { kbps: 60_000 }));
        assert_eq!(stats.bitrate_hints_sent, 1);
        assert_eq!(stats.bitrate_target_kbps, Some(60_000));
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn loss_before_the_host_ceiling_cuts_nothing_and_a_higher_host_is_hinted_down_once() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        rig.lose_a_frame(&mut packetizer);
        assert!(rig.loss_noticed());
        let unsteered = rig.counters.snapshot();
        rig.announce_ceiling(1, 150_000);

        let hint = rig.next_hint();
        let later = rig.control_messages_for(QUIET);
        let stats = rig.counters.snapshot();

        assert_eq!(unsteered.bitrate_hints_sent, 0);
        assert_eq!(unsteered.bitrate_target_kbps, None);
        assert_eq!(
            hint,
            Some(ControlMessage::BitrateHint { kbps: CLIENT_KBPS })
        );
        assert!(later.iter().all(|message| !matches!(
            message,
            ControlMessage::BitrateHint { kbps } if *kbps != CLIENT_KBPS
        )));
        assert_eq!(stats.bitrate_hints_sent, 1);
        assert_eq!(stats.bitrate_target_kbps, Some(CLIENT_KBPS));
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_host_ceiling_below_the_client_is_cut_from_the_hosts_bitrate() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        rig.announce_ceiling(1, 40_000);
        assert!(rig.wait_until(|rig| rig.counters.snapshot().bitrate_target_kbps == Some(40_000)));
        let before_loss = rig.counters.snapshot();
        rig.lose_a_frame(&mut packetizer);

        let hint = rig.next_hint();
        let stats = rig.counters.snapshot();

        assert_eq!(before_loss.bitrate_hints_sent, 0);
        assert_eq!(hint, Some(ControlMessage::BitrateHint { kbps: 30_000 }));
        assert_eq!(stats.bitrate_hints_sent, 1);
        assert_eq!(stats.bitrate_target_kbps, Some(30_000));
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_host_without_a_bitrate_to_steer_is_never_hinted() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        rig.announce_ceiling(1, 0);
        rig.lose_a_frame(&mut packetizer);
        assert!(rig.loss_noticed());

        let sent = rig.control_messages_for(QUIET);
        let stats = rig.counters.snapshot();

        assert!(
            sent.iter()
                .all(|message| !matches!(message, ControlMessage::BitrateHint { .. }))
        );
        assert_eq!(stats.bitrate_hints_sent, 0);
        assert_eq!(stats.bitrate_target_kbps, None);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_loss_after_decoded_frames_asks_to_recover_from_the_last_good_one() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        decoded_through(&rig, &mut packetizer, 3);
        assert!(rig.wait_until(|rig| rig.counters.snapshot().frame_acks_sent >= 1));
        packetizer
            .packetize(3, false, &video_frame(0, 64, 3))
            .unwrap();
        rig.send(
            &packetizer
                .packetize(4, false, &video_frame(0, 64, 4))
                .unwrap(),
        );
        let request = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::RequestRecovery { .. })
        });
        let Some(ControlMessage::RequestRecovery { frame, last_good }) = request else {
            panic!("no recovery request in {request:?}");
        };
        rig.send(
            &packetizer
                .packetize(5, false, &recovery_frame(0, Some(3), 64, 5))
                .unwrap(),
        );
        rig.send(
            &packetizer
                .packetize(6, false, &recovery_frame(0, Some(last_good), 64, 6))
                .unwrap(),
        );
        let recovered = rig.next_video();
        let skipped = rig.decode.recv_timeout(Duration::from_millis(100));
        let stats = rig.counters.snapshot();

        assert_eq!(frame, 3);
        assert!((1..=2).contains(&last_good));
        assert_eq!(recovered.number, 6);
        assert_eq!(recovered.recovers_from, Some(last_good));
        assert!(skipped.is_err());
        assert_eq!(stats.recovery_requests_sent, 1);
        assert_eq!(stats.reference_recoveries, 1);
        assert_eq!(stats.frames_skipped, 2);
        assert!(rig.finish().is_ok());
    }

    fn rig_that_never_times_a_frame_out() -> Rig {
        let network = NetworkConfig {
            reassembly_timeout_ms: u16::MAX,
            ..NetworkConfig::default()
        };

        Rig::start_on(network, None, None)
    }

    #[test]
    fn a_frame_lost_beyond_parity_asks_for_recovery_before_another_frame_is_sent() {
        let rig = rig_that_never_times_a_frame_out();
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::for_overhead(4, 25));

        rig.describe(0, description(1, 1280));
        decoded_through(&rig, &mut packetizer, 2);
        assert!(rig.wait_until(|rig| rig.counters.snapshot().frame_acks_sent >= 1));
        let datagrams = packetizer
            .packetize(2, false, &video_frame(0, 4_000, 2))
            .unwrap();
        rig.send(&datagrams[2..]);
        let request = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::RequestRecovery { .. })
        });
        let stats = rig.counters.snapshot();

        assert_eq!(datagrams.len(), 4);
        assert!(matches!(
            request,
            Some(ControlMessage::RequestRecovery { frame: 2, .. })
        ));
        assert_eq!(stats.frames_lost, 1);
        assert_eq!(stats.recovery_requests_sent, 1);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_frame_missing_its_tail_asks_for_recovery_once_the_next_frame_starts() {
        let rig = rig_that_never_times_a_frame_out();
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::for_overhead(4, 25));

        rig.describe(0, description(1, 1280));
        decoded_through(&rig, &mut packetizer, 2);
        assert!(rig.wait_until(|rig| rig.counters.snapshot().frame_acks_sent >= 1));
        let cut_short = packetizer
            .packetize(2, false, &video_frame(0, 4_000, 2))
            .unwrap();
        let next = packetizer
            .packetize(3, false, &video_frame(0, 4_000, 3))
            .unwrap();
        rig.send(&cut_short[..1]);
        let premature = rig.decode.recv_timeout(Duration::from_millis(100));
        let lost_while_unfinished = rig.counters.snapshot().frames_lost;
        rig.send(&next[..1]);
        let request = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::RequestRecovery { .. })
        });
        let stats = rig.counters.snapshot();

        assert!(premature.is_err());
        assert_eq!(lost_while_unfinished, 0);
        assert!(matches!(
            request,
            Some(ControlMessage::RequestRecovery { frame: 2, .. })
        ));
        assert_eq!(stats.frames_lost, 1);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn decoded_frames_are_acknowledged_to_the_host() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        decoded_through(&rig, &mut packetizer, 1);
        let ack = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::AcknowledgeFrame { .. })
        });

        assert_eq!(ack, Some(ControlMessage::AcknowledgeFrame { frame: 0 }));
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn an_unanswered_recovery_request_is_retried_as_a_keyframe_request() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        decoded_through(&rig, &mut packetizer, 2);
        packetizer
            .packetize(2, false, &video_frame(0, 64, 2))
            .unwrap();
        rig.send(
            &packetizer
                .packetize(3, false, &video_frame(0, 64, 3))
                .unwrap(),
        );
        let retry = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::RequestKeyframe { .. })
        });
        let stats = rig.counters.snapshot();

        assert_eq!(retry, Some(ControlMessage::RequestKeyframe { frame: 2 }));
        assert_eq!(stats.recovery_requests_sent, 1);
        assert!(stats.keyframe_requests_sent >= 1);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_recovery_the_decoder_rejects_asks_for_a_keyframe() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        decoded_through(&rig, &mut packetizer, 2);
        packetizer
            .packetize(2, false, &video_frame(0, 64, 2))
            .unwrap();
        rig.send(
            &packetizer
                .packetize(3, false, &video_frame(0, 64, 3))
                .unwrap(),
        );
        let recovery = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::RequestRecovery { .. })
        });
        let Some(ControlMessage::RequestRecovery { last_good, .. }) = recovery else {
            panic!("no recovery request in {recovery:?}");
        };
        rig.send(
            &packetizer
                .packetize(4, false, &recovery_frame(0, Some(last_good), 64, 4))
                .unwrap(),
        );
        let rejected = rig.next_video();
        rig.feedback
            .send(Feedback::DecodeFailed {
                frame: rejected.number,
            })
            .unwrap();
        let keyframe = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::RequestKeyframe { .. })
        });
        let stats = rig.counters.snapshot();

        assert!(matches!(
            keyframe,
            Some(ControlMessage::RequestKeyframe { .. })
        ));
        assert_eq!(stats.recovery_requests_sent, 1);
        assert!(stats.keyframe_requests_sent >= 1);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_decode_failure_asks_for_a_keyframe_and_drops_the_following_frames() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        decoded_through(&rig, &mut packetizer, 2);
        rig.feedback
            .send(Feedback::DecodeFailed { frame: 1 })
            .unwrap();
        let keyframe = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::RequestKeyframe { .. })
        });
        rig.send(
            &packetizer
                .packetize(2, false, &recovery_frame(0, Some(1), 64, 2))
                .unwrap(),
        );
        rig.send(
            &packetizer
                .packetize(3, false, &video_frame(0, 64, 3))
                .unwrap(),
        );
        rig.send(
            &packetizer
                .packetize(4, true, &video_frame(0, 64, 4))
                .unwrap(),
        );
        let resumed = rig.next_video();
        let stats = rig.counters.snapshot();

        assert_eq!(keyframe, Some(ControlMessage::RequestKeyframe { frame: 1 }));
        assert_eq!(stats.recovery_requests_sent, 0);
        assert!(stats.keyframe_requests_sent >= 1);
        assert!(resumed.frame.keyframe);
        assert_eq!(resumed.number, 4);
        assert!(stats.frames_skipped >= 2);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn the_last_good_frame_never_moves_back_and_a_recovery_needs_it() {
        let mut resync = Resync::default();

        resync.decoded(5);
        resync.decoded(4);
        resync.last_keyframe = Some(2);

        assert_eq!(resync.last_good, Some(5));
        assert!(resync.can_resume_from(5));
        assert!(resync.can_resume_from(2));
        assert!(!resync.can_resume_from(6));
        assert!(!resync.can_resume_from(1));
        assert!(!Resync::default().can_resume_from(0));
    }

    #[test]
    fn a_frame_missing_a_fragment_is_counted_lost_exactly_once() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        rig.send(
            &packetizer
                .packetize(0, true, &video_frame(0, 64, 0))
                .unwrap(),
        );
        rig.next_video();

        let mut fragments = packetizer
            .packetize(1, false, &video_frame(0, 4_000, 1))
            .unwrap();
        fragments.remove(1);
        rig.send(&fragments);
        rig.send(
            &packetizer
                .packetize(2, true, &video_frame(0, 64, 2))
                .unwrap(),
        );
        rig.next_video();

        assert_eq!(rig.counters.snapshot().frames_lost, 1);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn frames_that_never_arrived_are_added_to_the_loss_of_a_partial_one() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        rig.send(
            &packetizer
                .packetize(0, true, &video_frame(0, 64, 0))
                .unwrap(),
        );
        rig.next_video();

        packetizer
            .packetize(1, false, &video_frame(0, 64, 1))
            .unwrap();
        let mut fragments = packetizer
            .packetize(2, false, &video_frame(0, 4_000, 2))
            .unwrap();
        fragments.remove(1);
        rig.send(&fragments);
        rig.send(
            &packetizer
                .packetize(3, true, &video_frame(0, 64, 3))
                .unwrap(),
        );
        rig.next_video();

        assert_eq!(rig.counters.snapshot().frames_lost, 2);
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn video_frames_carry_their_capture_time_and_lose_the_header() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        rig.send(
            &packetizer
                .packetize(0, true, &video_frame(123_456, 64, 3))
                .unwrap(),
        );
        let video = rig.next_video();

        assert_eq!(video.captured_micros, 123_456);
        assert_eq!(video.frame.timestamp, Timestamp::from_micros(123_456));
        assert_eq!(video.frame.data, payload(64, 3));
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_video_frame_too_short_for_its_header_is_malformed_and_asks_for_a_keyframe() {
        let rig = Rig::start(None);
        let mut packetizer = rig.packetizer(StreamKind::Video, FecScheme::None);

        rig.describe(0, description(1, 1280));
        rig.send(&packetizer.packetize(0, true, &[1, 2, 3]).unwrap());

        assert!(rig.wait_until(|rig| rig.counters.snapshot().malformed_datagrams == 1));
        assert!(rig.wait_until(|rig| rig.counters.snapshot().keyframe_requests_sent == 1));
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn a_clock_pong_sets_the_host_clock_offset() {
        let rig = Rig::start(None);
        let pong = ControlPacket::Message {
            id: 0,
            message: ControlMessage::ClockPong {
                sent_micros: 0,
                host_micros: 5_000_000,
            },
        };

        rig.send(&[pong.to_datagram().unwrap()]);

        assert!(rig.wait_until(|rig| rig.counters.snapshot().clock_offset_micros.is_some()));
        let offset = rig.counters.snapshot().clock_offset_micros.unwrap();
        assert!((4_000_000..=5_000_000).contains(&offset));
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

        rig.send(
            &packetizer
                .packetize(0, true, &video_frame(0, 64, 0))
                .unwrap(),
        );
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
    fn a_wanted_quality_request_follows_the_hello_and_reports_reach_the_stats() {
        let rig = Rig::start(None);
        let report = rusplay_protocol::QualityReport {
            frame: 12,
            psnr: rusplay_protocol::QualityPlanes {
                luma: rusplay_protocol::QualityDecibels(4_000),
                cb: rusplay_protocol::QualityDecibels(4_100),
                cr: rusplay_protocol::QualityDecibels(4_200),
            },
            psnr_overall: rusplay_protocol::QualityDecibels(4_050),
            xpsnr: rusplay_protocol::QualityPlanes {
                luma: rusplay_protocol::QualityDecibels(3_900),
                cb: rusplay_protocol::QualityDecibels(4_000),
                cr: rusplay_protocol::QualityDecibels(4_100),
            },
        };
        let hello = ControlMessage::Hello {
            version: ProtocolVersion::CURRENT,
            formats: FormatSet::ALL,
        };

        rig.counters.quality.want(true);
        rig.send(&[ControlPacket::Message {
            id: 0,
            message: hello,
        }
        .to_datagram()
        .unwrap()]);
        let request = next_message_matching(&rig, |message| {
            matches!(message, ControlMessage::QualityRequest { .. })
        });
        rig.send(&[ControlPacket::Message {
            id: 1,
            message: ControlMessage::QualityReport(report),
        }
        .to_datagram()
        .unwrap()]);
        let reported =
            rig.wait_until(|rig| rig.counters.snapshot().quality == crate::QualityState::Reported);
        let stats = rig.counters.snapshot();

        assert_eq!(
            request,
            Some(ControlMessage::QualityRequest { enabled: true })
        );
        assert!(reported);
        assert_eq!(stats.quality_report, Some(report));
        assert!(stats.quality_age.is_some());
        rig.finish().unwrap();
    }

    fn clipboard_rig(send: bool, receive: bool, board: &rusplay_clipboard::MemoryClipboard) -> Rig {
        let policy = rusplay_clipboard::ClipboardPolicy {
            send,
            receive,
            max_bytes: 4096,
        };
        let link = ClipboardLink::new(policy, Box::new(board.clone()));
        let rig = Rig::start_with(None, Some(link));
        let hello = ControlMessage::Hello {
            version: ProtocolVersion::CURRENT,
            formats: FormatSet::ALL,
        };

        rig.send(&[ControlPacket::Message {
            id: 0,
            message: hello,
        }
        .to_datagram()
        .unwrap()]);
        rig
    }

    fn clipboard_datagrams(first_id: u32, transfer: u32, text: &str) -> Vec<Vec<u8>> {
        ClipboardChunk::split(transfer, text)
            .unwrap()
            .into_iter()
            .zip(first_id..)
            .map(|(chunk, id)| {
                ControlPacket::Message {
                    id,
                    message: ControlMessage::Clipboard(chunk),
                }
                .to_datagram()
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn clipboard_text_from_the_host_reaches_the_client_clipboard() {
        let board = rusplay_clipboard::MemoryClipboard::default();
        let rig = clipboard_rig(false, true, &board);
        let text = "über ".repeat(200);

        rig.send(&clipboard_datagrams(1, 0, &text));

        assert!(rig.wait_until(|_| board.contents().as_deref() == Some(text.as_str())));
        rig.finish().unwrap();
    }

    #[test]
    fn clipboard_text_copied_on_the_client_is_sent_to_the_host_in_chunks() {
        let board = rusplay_clipboard::MemoryClipboard::default();
        let rig = clipboard_rig(true, false, &board);
        let text = "hello ".repeat(300);

        board.copy(text.clone());
        let mut reassembler = rusplay_protocol::ClipboardReassembler::new(4096);
        let mut received = None;
        while received.is_none() {
            let Some(ControlMessage::Clipboard(chunk)) = next_message_matching(&rig, |message| {
                matches!(message, ControlMessage::Clipboard(_))
            }) else {
                break;
            };
            received = reassembler.push(&chunk).unwrap();
        }

        assert_eq!(received.as_deref(), Some(text.as_str()));
        rig.finish().unwrap();
    }

    #[test]
    fn clipboard_text_is_ignored_when_the_client_may_not_receive() {
        let board = rusplay_clipboard::MemoryClipboard::default();
        let rig = clipboard_rig(true, false, &board);

        rig.send(&clipboard_datagrams(1, 0, "not for the client"));
        thread::sleep(Duration::from_millis(200));

        assert_eq!(board.writes(), 0);
        rig.finish().unwrap();
    }

    #[test]
    fn invalid_clipboard_text_is_counted_malformed_and_never_applied() {
        let board = rusplay_clipboard::MemoryClipboard::default();
        let rig = clipboard_rig(false, true, &board);
        let chunk = ClipboardChunk::new(0, 0, 1, &[0xC3, 0x28]).unwrap();

        rig.send(&[ControlPacket::Message {
            id: 1,
            message: ControlMessage::Clipboard(chunk),
        }
        .to_datagram()
        .unwrap()]);

        assert!(rig.wait_until(|rig| rig.counters.snapshot().malformed_datagrams >= 1));
        assert_eq!(board.writes(), 0);
        rig.finish().unwrap();
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
                PadFeedback::Rumble(RumbleCommand {
                    slot: 1,
                    strong: 0xFFFF,
                    weak: 0x4000,
                    delay_ms: 3,
                    duration_ms: 250,
                }),
                PadFeedback::Rumble(RumbleCommand::stop(1)),
            ]
        );
        assert!(rig.finish().is_ok());
    }

    #[test]
    fn lightbar_led_and_trigger_messages_are_forwarded_as_pad_feedback() {
        let rig = Rig::start(None);
        let messages = [
            ControlMessage::Lightbar {
                slot: 2,
                red: 1,
                green: 2,
                blue: 3,
            },
            ControlMessage::PlayerLeds { slot: 2, mask: 9 },
            ControlMessage::TriggerEffect {
                slot: 2,
                right: false,
                effect: [4; 11],
            },
        ];

        for (id, message) in messages.into_iter().enumerate() {
            let packet = ControlPacket::Message {
                id: u32::try_from(id).unwrap(),
                message,
            };
            rig.send(&[packet.to_datagram().unwrap()]);
        }

        let received: Vec<_> = (0..3)
            .map(|_| rig.rumble.recv_timeout(WAIT).expect("feedback arrives"))
            .collect();
        assert_eq!(
            received,
            vec![
                PadFeedback::Lightbar {
                    slot: 2,
                    red: 1,
                    green: 2,
                    blue: 3
                },
                PadFeedback::PlayerLeds { slot: 2, mask: 9 },
                PadFeedback::TriggerEffect {
                    slot: 2,
                    right: false,
                    effect: [4; 11]
                },
            ]
        );
        assert!(rig.finish().is_ok());
    }
}
