use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use rusplay_config::NetworkConfig;
use rusplay_net::{ControlEvent, NetError, RecvBatch, Transport};
use rusplay_protocol::{ControlMessage, PacketHeader, ProtocolVersion, StreamKind};

use crate::{HostError, accept::Replay, outbox::ControlOutbox, shared::Shared};

const RECV_BATCH_DATAGRAMS: usize = 32;
const IDLE_WAKE: Duration = Duration::from_millis(20);
const GOODBYE_LINGER_RETRANSMITS: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Ended,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ControlTiming {
    pub handshake_timeout: Duration,
    pub peer_timeout: Duration,
    pub keepalive_interval: Duration,
    pub goodbye_linger: Duration,
}

impl From<&NetworkConfig> for ControlTiming {
    fn from(network: &NetworkConfig) -> Self {
        let millis = |value: u32| Duration::from_millis(u64::from(value));

        Self {
            handshake_timeout: millis(network.handshake_timeout_ms),
            peer_timeout: millis(network.peer_timeout_ms),
            keepalive_interval: millis(network.keepalive_interval_ms),
            goodbye_linger: millis(u32::from(network.control_retransmit_ms))
                * GOODBYE_LINGER_RETRANSMITS,
        }
    }
}

pub(crate) struct ControlLinks {
    pub shared: Arc<Shared>,
    pub transport: Arc<Transport>,
    pub client: SocketAddr,
    pub replay: Replay,
    pub outbox: Arc<ControlOutbox>,
}

pub(crate) struct ControlStage {
    timing: ControlTiming,
    links: ControlLinks,
    streaming: bool,
    started: Instant,
    last_heard: Instant,
    next_keepalive: Instant,
}

impl ControlStage {
    pub(crate) fn new(network: &NetworkConfig, links: ControlLinks) -> Self {
        let now = Instant::now();
        let timing = ControlTiming::from(network);

        Self {
            timing,
            links,
            streaming: false,
            started: now,
            last_heard: now,
            next_keepalive: now + timing.keepalive_interval,
        }
    }

    pub(crate) fn run(mut self) -> Result<(), HostError> {
        let result = self.serve();
        self.links.shared.stop();

        result
    }

    fn serve(&mut self) -> Result<(), HostError> {
        let mut batch = RecvBatch::new(
            RECV_BATCH_DATAGRAMS,
            usize::from(self.links.transport.mtu()),
        );
        let mut events = Vec::new();

        loop {
            if self.links.shared.stopped() {
                self.say_goodbye();
                return Ok(());
            }

            let wait = self
                .next_wake(Instant::now())
                .saturating_duration_since(Instant::now());
            self.links.transport.recv_batch_timeout(&mut batch, wait)?;
            let now = Instant::now();

            for (datagram, from) in batch.iter() {
                if from == self.links.client {
                    self.last_heard = now;
                    self.route(datagram, &mut events)?;
                }
            }
            self.links.outbox.channel().poll(now, &mut events);

            if self.handle(&mut events, now)? == Flow::Ended {
                return Ok(());
            }
            self.keep_alive(now)?;

            self.links.outbox.send_announcement(now)?;
            self.links.outbox.flush()?;
            self.links
                .shared
                .counters
                .rejected_datagrams
                .set(self.links.transport.rejected_datagrams());

            if !self.streaming
                && now.saturating_duration_since(self.started) >= self.timing.handshake_timeout
            {
                return Err(HostError::HandshakeTimeout(self.timing.handshake_timeout));
            }
            if now.saturating_duration_since(self.last_heard) >= self.timing.peer_timeout {
                return Err(HostError::ClientSilent(self.timing.peer_timeout));
            }
        }
    }

    fn next_wake(&self, now: Instant) -> Instant {
        let handshake_deadline =
            (!self.streaming).then_some(self.started + self.timing.handshake_timeout);
        let keepalive = self.streaming.then_some(self.next_keepalive);

        [
            self.links.outbox.channel().next_deadline(),
            handshake_deadline,
            keepalive,
            Some(self.last_heard + self.timing.peer_timeout),
        ]
        .into_iter()
        .flatten()
        .fold(now + IDLE_WAKE, Instant::min)
    }

    fn route(&mut self, datagram: &[u8], events: &mut Vec<ControlEvent>) -> Result<(), HostError> {
        let accepted = match PacketHeader::read(datagram).map(|header| header.stream) {
            Ok(StreamKind::Control) => self
                .links
                .outbox
                .channel()
                .handle_datagram(datagram, events)
                .is_ok(),
            Ok(StreamKind::Pairing | StreamKind::Reconnect) => {
                if let Some(reply) = self.links.replay.answer(datagram) {
                    self.links.transport.send_to(reply, self.links.client)?;
                }
                true
            }
            Ok(_) => true,
            Err(_) => false,
        };

        if !accepted {
            self.links.shared.counters.malformed_datagrams.increment();
        }
        Ok(())
    }

    fn handle(&mut self, events: &mut Vec<ControlEvent>, now: Instant) -> Result<Flow, HostError> {
        let shared = Arc::clone(&self.links.shared);
        let mut flow = Flow::Continue;

        for event in events.drain(..) {
            match event {
                ControlEvent::Received(ControlMessage::Hello { version }) => {
                    if version != ProtocolVersion::CURRENT {
                        self.say_goodbye();
                        return Err(HostError::VersionMismatch {
                            ours: ProtocolVersion::CURRENT.0,
                            theirs: version.0,
                        });
                    }
                    self.send(
                        ControlMessage::Hello {
                            version: ProtocolVersion::CURRENT,
                        },
                        now,
                    )?;
                    if !self.streaming {
                        self.streaming = true;
                        self.next_keepalive = now + self.timing.keepalive_interval;
                        shared.start();
                    }
                }
                ControlEvent::Received(ControlMessage::RequestKeyframe { frame }) => {
                    shared.mailbox.request_keyframe(frame);
                }
                ControlEvent::Received(ControlMessage::ReportLoss { count, .. }) => {
                    shared.counters.packets_reported_lost.add(u64::from(count));
                }
                ControlEvent::Received(ControlMessage::BitrateHint { kbps }) => {
                    shared.mailbox.set_bitrate(kbps);
                }
                ControlEvent::Received(ControlMessage::Goodbye) => flow = Flow::Ended,
                ControlEvent::Undeliverable {
                    message: ControlMessage::Hello { .. },
                    ..
                } => return Err(HostError::HelloUndeliverable),
                ControlEvent::Undeliverable {
                    message: ControlMessage::StreamFormat(description),
                    ..
                } => {
                    shared.counters.control_undeliverable.increment();
                    self.links.outbox.reannounce(&description);
                }
                ControlEvent::Undeliverable { .. } => {
                    shared.counters.control_undeliverable.increment();
                }
                ControlEvent::Received(
                    ControlMessage::Keepalive
                    | ControlMessage::Rumble { .. }
                    | ControlMessage::StreamFormat(_),
                )
                | ControlEvent::Acked { .. } => {}
            }
        }

        Ok(flow)
    }

    fn keep_alive(&mut self, now: Instant) -> Result<(), HostError> {
        if !self.streaming || now < self.next_keepalive {
            return Ok(());
        }

        self.next_keepalive = now + self.timing.keepalive_interval;
        if self.send(ControlMessage::Keepalive, now)? {
            self.links.shared.counters.keepalives_sent.increment();
        }
        Ok(())
    }

    fn send(&mut self, message: ControlMessage, now: Instant) -> Result<bool, HostError> {
        let sent = self.links.outbox.channel().send(message, now);
        match sent {
            Ok(_) => Ok(true),
            Err(NetError::ControlWindowFull { .. }) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn say_goodbye(&mut self) {
        let sent = self
            .links
            .outbox
            .channel()
            .send(ControlMessage::Goodbye, Instant::now());
        let Ok(goodbye) = sent else {
            return;
        };
        let deadline = Instant::now() + self.timing.goodbye_linger;
        let mut batch = RecvBatch::new(
            RECV_BATCH_DATAGRAMS,
            usize::from(self.links.transport.mtu()),
        );
        let mut events = Vec::new();

        while self.links.outbox.flush().is_ok() {
            let now = Instant::now();
            if now >= deadline || events.contains(&ControlEvent::Acked { id: goodbye }) {
                return;
            }

            let wait = self
                .links
                .outbox
                .channel()
                .next_deadline()
                .unwrap_or(deadline)
                .min(deadline)
                .saturating_duration_since(now);
            if self
                .links
                .transport
                .recv_batch_timeout(&mut batch, wait)
                .is_err()
            {
                return;
            }
            for (datagram, from) in batch.iter() {
                if from == self.links.client
                    && PacketHeader::read(datagram)
                        .is_ok_and(|header| header.stream == StreamKind::Control)
                {
                    let mut channel = self.links.outbox.channel();
                    channel.handle_datagram(datagram, &mut events).ok();
                }
            }
            self.links
                .outbox
                .channel()
                .poll(Instant::now(), &mut events);
        }
    }
}
