use std::{
    net::SocketAddr,
    ops::RangeInclusive,
    sync::{
        Arc,
        mpsc::{Sender, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use rusplay_config::{Config, NetworkConfig};
use rusplay_core::{CodecFormat, FormatSet};
use rusplay_net::{
    ControlEvent, NetError, PeerStore, RecvBatch, SessionTakeover, Transport, busy_reply,
};
use rusplay_protocol::{
    ControlMessage, FecHeader, PacketHeader, PeerId, ProtocolVersion, StreamKind,
};

use crate::{
    HostError,
    accept::{Replay, refresh_peers},
    negotiate::negotiate,
    outbox::ControlOutbox,
    shared::Shared,
};

const RECV_BATCH_DATAGRAMS: usize = 32;
const IDLE_WAKE: Duration = Duration::from_millis(20);
const GOODBYE_LINGER_RETRANSMITS: u32 = 4;

fn protects_microphone(datagram: &[u8]) -> bool {
    PacketHeader::parse(datagram)
        .ok()
        .and_then(|(_, payload)| FecHeader::parse(payload).ok())
        .is_some_and(|(fec, _)| fec.protects == StreamKind::Microphone)
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HintPolicy {
    Clamp(RangeInclusive<u32>),
    Ignore,
}

impl HintPolicy {
    pub(crate) fn from_config(config: &Config) -> Self {
        if config.video.encoder.is_constant_quality() {
            Self::Ignore
        } else {
            Self::Clamp(config.network.bitrate_hint_range())
        }
    }

    fn apply(&self, kbps: u32) -> Option<u32> {
        match self {
            Self::Clamp(range) => Some(kbps.clamp(*range.start(), *range.end())),
            Self::Ignore => None,
        }
    }
}

pub(crate) struct ControlLinks {
    pub shared: Arc<Shared>,
    pub transport: Arc<Transport>,
    pub peers: Arc<RwLock<PeerStore>>,
    pub peer: PeerId,
    pub replay: Replay,
    pub outbox: Arc<ControlOutbox>,
    pub input: Option<Sender<Vec<u8>>>,
    pub microphone: Option<SyncSender<Vec<u8>>>,
    pub configured: CodecFormat,
}

pub(crate) struct ControlStage {
    timing: ControlTiming,
    hints: HintPolicy,
    links: ControlLinks,
    takeover: SessionTakeover,
    streaming: bool,
    unanswered_hello: Option<FormatSet>,
    started: Instant,
    last_heard: Instant,
    next_keepalive: Instant,
}

impl ControlStage {
    pub(crate) fn new(network: &NetworkConfig, hints: HintPolicy, links: ControlLinks) -> Self {
        let now = Instant::now();
        let timing = ControlTiming::from(network);

        Self {
            timing,
            hints,
            takeover: SessionTakeover::new(links.peer),
            links,
            streaming: false,
            unanswered_hello: None,
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
                if from == self.client() {
                    self.last_heard = now;
                    self.route(datagram, from, now, &mut events)?;
                } else {
                    self.answer_stranger(datagram, from, now, &mut events)?;
                }
            }
            let rtt = {
                let mut channel = self.links.outbox.channel();
                channel.poll(now, &mut events);
                channel.rtt()
            };
            self.links.shared.counters.set_rtt(rtt);

            if self.handle(&mut events, now)? == Flow::Ended {
                return Ok(());
            }
            self.answer_hello(now)?;
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

    fn client(&self) -> SocketAddr {
        self.links.shared.client.address()
    }

    fn answer_stranger(
        &mut self,
        datagram: &[u8],
        from: SocketAddr,
        now: Instant,
        events: &mut Vec<ControlEvent>,
    ) -> Result<(), HostError> {
        let reconnecting =
            PacketHeader::read(datagram).is_ok_and(|header| header.stream == StreamKind::Reconnect);

        if reconnecting && same_family(from, self.client()) {
            self.take_over(datagram, from, now, events)
        } else {
            if let Some(refusal) = busy_reply(datagram) {
                self.links.transport.send_to(&refusal, from).ok();
            }
            Ok(())
        }
    }

    fn take_over(
        &mut self,
        datagram: &[u8],
        from: SocketAddr,
        now: Instant,
        events: &mut Vec<ControlEvent>,
    ) -> Result<(), HostError> {
        refresh_peers(&self.links.peers).ok();
        let handled = self
            .takeover
            .handle(datagram, from, &self.links.peers.read());
        let output = match handled {
            Ok(output) => output,
            Err(NetError::Protocol(_)) => return Ok(()),
            Err(error) => return Err(error.into()),
        };

        if let Some(accepted) = &output.accepted {
            self.links.transport.install_session(&accepted.keys);
        }
        if let Some(reply) = &output.reply {
            self.links.transport.send_to(reply, from).ok();
        }
        if output.accepted.is_some() {
            let replay = Replay {
                request: datagram.to_vec(),
                reply: output.reply.unwrap_or_default(),
            };
            self.switch_client(from, replay, now, events);
        }

        Ok(())
    }

    fn resume_after_takeover(
        &mut self,
        chosen: CodecFormat,
        encodable: FormatSet,
        decodable: FormatSet,
        now: Instant,
    ) -> Result<(), HostError> {
        if !decodable.contains(chosen) {
            self.say_goodbye();
            return Err(HostError::NoCommonFormat {
                encodable,
                decodable,
            });
        }

        self.streaming = true;
        self.next_keepalive = now + self.timing.keepalive_interval;
        self.links.outbox.resend_announcement(now)?;
        self.links.shared.start();
        Ok(())
    }

    fn switch_client(
        &mut self,
        address: SocketAddr,
        replay: Replay,
        now: Instant,
        events: &mut Vec<ControlEvent>,
    ) {
        let shared = &self.links.shared;
        shared.suspend();
        shared.client.switch(address);
        self.links.outbox.reset();
        shared.counters.sessions_taken_over.increment();

        events.clear();
        self.links.replay = replay;
        self.streaming = false;
        self.unanswered_hello = None;
        self.started = now;
        self.last_heard = now;
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

    fn route(
        &mut self,
        datagram: &[u8],
        from: SocketAddr,
        now: Instant,
        events: &mut Vec<ControlEvent>,
    ) -> Result<(), HostError> {
        let accepted = match PacketHeader::read(datagram).map(|header| header.stream) {
            Ok(StreamKind::Control) => self
                .links
                .outbox
                .channel()
                .handle_datagram(datagram, now, events)
                .is_ok(),
            Ok(StreamKind::Pairing) => {
                if let Some(reply) = self.links.replay.answer(datagram) {
                    self.links.transport.send_to(reply, from)?;
                }
                true
            }
            Ok(StreamKind::Reconnect) => {
                if let Some(reply) = self.links.replay.answer(datagram) {
                    self.links.transport.send_to(reply, from)?;
                } else {
                    self.take_over(datagram, from, now, events)?;
                }
                true
            }
            Ok(StreamKind::Input) => {
                if self.streaming
                    && let Some(input) = &self.links.input
                {
                    input.send(datagram.to_vec()).ok();
                }
                true
            }
            Ok(StreamKind::Microphone) => {
                self.forward_microphone(datagram);
                true
            }
            Ok(StreamKind::Fec) if protects_microphone(datagram) => {
                self.forward_microphone(datagram);
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

    fn forward_microphone(&self, datagram: &[u8]) {
        if !self.streaming {
            return;
        }
        let Some(microphone) = &self.links.microphone else {
            return;
        };

        if let Err(TrySendError::Full(_)) = microphone.try_send(datagram.to_vec()) {
            self.links
                .shared
                .counters
                .microphone_packets_dropped
                .increment();
        }
    }

    fn handle(&mut self, events: &mut Vec<ControlEvent>, now: Instant) -> Result<Flow, HostError> {
        let shared = Arc::clone(&self.links.shared);
        let mut flow = Flow::Continue;

        for event in events.drain(..) {
            match event {
                ControlEvent::Received(ControlMessage::Hello { version, formats }) => {
                    if version != ProtocolVersion::CURRENT {
                        self.say_goodbye();
                        return Err(HostError::VersionMismatch {
                            ours: ProtocolVersion::CURRENT.0,
                            theirs: version.0,
                        });
                    }
                    self.unanswered_hello = Some(formats);
                }
                ControlEvent::Received(ControlMessage::RequestKeyframe { frame }) => {
                    shared.mailbox.request_keyframe(frame);
                }
                ControlEvent::Received(ControlMessage::ReportLoss { count, .. }) => {
                    shared.counters.packets_reported_lost.add(u64::from(count));
                }
                ControlEvent::Received(ControlMessage::BitrateHint { kbps }) => {
                    self.take_hint(kbps);
                }
                ControlEvent::Received(ControlMessage::ClockPing { sent_micros }) => {
                    let pong = ControlMessage::ClockPong {
                        sent_micros,
                        host_micros: shared.clock.micros(),
                    };
                    self.send(pong, now)?;
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
                    | ControlMessage::StreamFormat(_)
                    | ControlMessage::ClockPong { .. },
                )
                | ControlEvent::Acked { .. } => {}
            }
        }

        Ok(flow)
    }

    fn answer_hello(&mut self, now: Instant) -> Result<(), HostError> {
        let shared = Arc::clone(&self.links.shared);
        let (Some(decodable), Some(encodable)) =
            (self.unanswered_hello, shared.negotiation.encodable())
        else {
            return Ok(());
        };
        self.unanswered_hello = None;

        self.send(
            ControlMessage::Hello {
                version: ProtocolVersion::CURRENT,
                formats: encodable,
            },
            now,
        )?;
        if self.streaming {
            return Ok(());
        }

        if let Some(agreed) = shared.negotiation.agreed() {
            return self.resume_after_takeover(agreed.chosen, encodable, decodable, now);
        }

        match negotiate(self.links.configured, encodable, decodable) {
            Ok(negotiated) => {
                shared.negotiation.agree(negotiated);
                self.streaming = true;
                self.next_keepalive = now + self.timing.keepalive_interval;
                shared.start();
                Ok(())
            }
            Err(error) => {
                self.say_goodbye();
                Err(error)
            }
        }
    }

    fn take_hint(&self, kbps: u32) {
        let counters = &self.links.shared.counters;
        let Some(applied) = self.hints.apply(kbps) else {
            counters.bitrate_hints_ignored.increment();
            return;
        };

        if applied != kbps {
            counters.bitrate_hints_clamped.increment();
        }
        counters.set_bitrate_target(applied);
        self.links.shared.mailbox.set_bitrate(applied);
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
                if from == self.client()
                    && PacketHeader::read(datagram)
                        .is_ok_and(|header| header.stream == StreamKind::Control)
                {
                    let mut channel = self.links.outbox.channel();
                    channel
                        .handle_datagram(datagram, Instant::now(), &mut events)
                        .ok();
                }
            }
            self.links
                .outbox
                .channel()
                .poll(Instant::now(), &mut events);
        }
    }
}

fn same_family(first: SocketAddr, second: SocketAddr) -> bool {
    first.is_ipv4() == second.is_ipv4()
}

#[cfg(test)]
mod tests {
    use rusplay_net::{FecScheme, Packetizer};

    use super::*;

    #[test]
    fn a_takeover_never_changes_the_address_family() {
        let v4 = SocketAddr::from(([127, 0, 0, 1], 1));
        let v6 = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 1));

        assert!(same_family(v4, SocketAddr::from(([10, 0, 0, 2], 9))));
        assert!(!same_family(v4, v6));
    }

    const MAX_DATAGRAM: usize = 1200;
    const PARITY_SCHEME: FecScheme = FecScheme::ReedSolomon {
        data_shards: 2,
        parity_shards: 1,
    };

    fn datagrams(stream: StreamKind) -> Vec<Vec<u8>> {
        Packetizer::new(stream, MAX_DATAGRAM, PARITY_SCHEME)
            .unwrap()
            .packetize(0, false, &[7; 3000])
            .unwrap()
    }

    fn parity_of(stream: StreamKind) -> Vec<Vec<u8>> {
        datagrams(stream)
            .into_iter()
            .filter(|datagram| {
                PacketHeader::read(datagram).is_ok_and(|header| header.stream == StreamKind::Fec)
            })
            .collect()
    }

    #[test]
    fn parity_for_the_microphone_is_routed_to_it() {
        let parity = parity_of(StreamKind::Microphone);

        assert_ne!(parity.len(), 0);
        assert!(parity.iter().all(|datagram| protects_microphone(datagram)));
    }

    #[test]
    fn parity_for_other_streams_and_plain_data_are_not() {
        let audio = parity_of(StreamKind::Audio);
        let data = datagrams(StreamKind::Microphone);

        assert_ne!(audio.len(), 0);
        assert!(audio.iter().all(|datagram| !protects_microphone(datagram)));
        assert!(!protects_microphone(&data[0]));
        assert!(!protects_microphone(&[0; 4]));
    }
}
