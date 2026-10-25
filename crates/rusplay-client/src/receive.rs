use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use rusplay_config::NetworkConfig;
use rusplay_core::{Codec, EncodedFrame, Timestamp};
use rusplay_net::{
    ControlChannel, ControlConfig, ControlEvent, NetError, ReassembledFrame, Reassembler,
    ReassemblyEvent, ReassemblyLimits, RecvBatch, Transport,
};
use rusplay_protocol::{ControlMessage, FecHeader, PacketHeader, ProtocolVersion, StreamKind};

use crate::{ClientError, stats::Counters};

const RECV_BATCH_DATAGRAMS: usize = 64;
const IDLE_WAKE: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(crate) struct EncodedVideo {
    pub frame: EncodedFrame,
    pub number: u32,
    pub received_at: Instant,
}

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
pub(crate) struct Links {
    pub decode_queue: SyncSender<EncodedVideo>,
    pub feedback: Receiver<Feedback>,
    pub handshake: Sender<()>,
    pub stop: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
}

#[derive(Debug)]
pub(crate) struct ReceiveStage {
    transport: Transport,
    host: SocketAddr,
    codec: Codec,
    timing: Timing,
    control: ControlChannel,
    video: Reassembler,
    links: Links,
    handshake_done: bool,
    resync: Resync,
    started: Instant,
    last_heard: Instant,
}

impl ReceiveStage {
    pub(crate) fn new(
        transport: Transport,
        host: SocketAddr,
        codec: Codec,
        network: &NetworkConfig,
        links: Links,
    ) -> Self {
        let now = Instant::now();

        Self {
            transport,
            host,
            codec,
            timing: Timing {
                peer_timeout: Duration::from_millis(u64::from(network.peer_timeout_ms)),
                keyframe_interval: Duration::from_millis(u64::from(
                    network.keyframe_request_interval_ms,
                )),
            },
            control: ControlChannel::new(ControlConfig::from(network)),
            video: Reassembler::new(StreamKind::Video, ReassemblyLimits::from(network)),
            links,
            handshake_done: false,
            resync: Resync::default(),
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
        let mut events = Vec::new();
        let mut control_events = Vec::new();

        self.control.send(
            ControlMessage::Hello {
                version: ProtocolVersion::CURRENT,
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
                    self.route(datagram, now, &mut events, &mut control_events);
                }
            }

            self.video.poll_timeouts(now, &mut events);
            self.control.poll(now, &mut control_events);

            let reassembly = self.handle_reassembly(&mut events, now)?;
            let control = self.handle_control(&mut control_events)?;
            if reassembly == Flow::Ended || control == Flow::Ended {
                return Ok(());
            }

            self.drain_feedback(now)?;
            if self.resync.retry_due(now, self.timing.keyframe_interval) {
                self.send_keyframe_request(now)?;
            }

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

    fn next_wake(&self, now: Instant) -> Instant {
        [
            self.control.next_deadline(),
            self.video.next_timeout(),
            self.resync.next_retry(self.timing.keyframe_interval),
            Some(self.last_heard + self.timing.peer_timeout),
        ]
        .into_iter()
        .flatten()
        .fold(now + IDLE_WAKE, Instant::min)
    }

    fn route(
        &mut self,
        datagram: &[u8],
        now: Instant,
        events: &mut Vec<ReassemblyEvent>,
        control_events: &mut Vec<ControlEvent>,
    ) {
        let accepted = match PacketHeader::parse(datagram) {
            Ok((header, _)) if header.stream == StreamKind::Control => self
                .control
                .handle_datagram(datagram, control_events)
                .is_ok(),
            Ok((header, _)) if header.stream == StreamKind::Video => {
                self.video.push(datagram, now, events).is_ok()
            }
            Ok((header, payload)) if header.stream == StreamKind::Fec => {
                match FecHeader::parse(payload) {
                    Ok((fec, _)) if fec.protects == StreamKind::Video => {
                        self.video.push(datagram, now, events).is_ok()
                    }
                    Ok(_) => true,
                    Err(_) => false,
                }
            }
            Ok(_) => true,
            Err(_) => false,
        };

        if !accepted {
            self.links.counters.malformed_datagrams.increment();
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
                    if !self.resync.predates_last_keyframe(frame) {
                        self.request_keyframe(frame, now)?;
                    }
                }
                ReassemblyEvent::PacketsMissing {
                    first_sequence,
                    count,
                } => {
                    counters.packets_missing.add(u64::from(count));
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
            if !frame.keyframe {
                self.request_keyframe(frame.frame.wrapping_sub(skipped), now)?;
            }
        }

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
                codec: self.codec,
                timestamp: timestamp_since(self.started, now),
                keyframe: frame.keyframe,
                data: frame.data,
            },
            number,
            received_at: now,
        };

        match self.links.decode_queue.try_send(video) {
            Ok(()) => Ok(Flow::Continue),
            Err(TrySendError::Full(_)) => {
                self.links.counters.frames_dropped.increment();
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
                ControlEvent::Received(ControlMessage::Hello { version }) => {
                    if version != ProtocolVersion::CURRENT {
                        return Err(ClientError::VersionMismatch {
                            ours: ProtocolVersion::CURRENT.0,
                            theirs: version.0,
                        });
                    }
                    if !self.handshake_done {
                        self.handshake_done = true;
                        self.links.handshake.send(()).ok();
                    }
                }
                ControlEvent::Received(ControlMessage::Goodbye) => flow = Flow::Ended,
                ControlEvent::Undeliverable {
                    message: ControlMessage::Hello { .. },
                    ..
                } if !self.handshake_done => return Err(ClientError::HelloUndeliverable),
                ControlEvent::Undeliverable { .. } => {
                    self.links.counters.control_undeliverable.increment();
                }
                ControlEvent::Received(_) | ControlEvent::Acked { .. } => {}
            }
        }

        Ok(flow)
    }

    fn drain_feedback(&mut self, now: Instant) -> Result<(), ClientError> {
        while let Ok(feedback) = self.links.feedback.try_recv() {
            match feedback {
                Feedback::DecodeFailed { frame } => self.request_keyframe(frame, now)?,
            }
        }

        Ok(())
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
