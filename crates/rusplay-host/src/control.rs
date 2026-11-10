use std::{
    net::SocketAddr,
    ops::RangeInclusive,
    sync::{
        Arc,
        mpsc::{SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use parking_lot::RwLock;
use rusplay_clipboard::{ClipboardError, ClipboardLink};
use rusplay_config::{Config, NetworkConfig};
use rusplay_core::{CodecFormat, FormatSet};
use rusplay_net::{
    ControlEvent, NetError, PeerStore, ReconnectTiming, RecvBatch, SessionTakeover, Transport,
    busy_reply, proves_peer_liveness,
};
use rusplay_protocol::{
    ClientDisplay, ClipboardChunk, ControlMessage, FecHeader, PacketHeader, PeerId,
    ProtocolVersion, StreamKind,
};

use crate::{
    HostError,
    accept::{Replay, refresh_peers},
    datagrams::ForwardedDatagram,
    mailbox::RecoveryRequest,
    negotiate::{StreamChoice, negotiate},
    outbox::ControlOutbox,
    shared::Shared,
    source::CaptureFeed,
    virtual_output::{ResizeGate, VirtualOutput, select_output, stream_rate},
};

const RECV_BATCH_DATAGRAMS: usize = 32;
const IDLE_WAKE: Duration = Duration::from_millis(20);
const GOODBYE_LINGER_RETRANSMITS: u32 = 4;
const UNSTEERED_CEILING_KBPS: u32 = 0;

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
            let ceiling = config.video.bitrate_kbps;
            let floor = config.network.bitrate_hint_min_kbps.min(ceiling);

            Self::Clamp(floor..=ceiling)
        }
    }

    pub(crate) fn ceiling_kbps(&self) -> u32 {
        match self {
            Self::Clamp(range) => *range.end(),
            Self::Ignore => UNSTEERED_CEILING_KBPS,
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
    pub input: Option<SyncSender<ForwardedDatagram>>,
    pub microphone: Option<SyncSender<ForwardedDatagram>>,
    pub stream: StreamChoice,
    pub source: Arc<CaptureFeed>,
    pub follow_refresh: Option<u32>,
    pub source_wait: Duration,
    pub virtual_output: Option<VirtualOutput>,
    pub clipboard: Option<ClipboardLink>,
}

pub(crate) struct ControlStage {
    timing: ControlTiming,
    hints: HintPolicy,
    links: ControlLinks,
    takeover: SessionTakeover,
    streaming: bool,
    unanswered_hello: Option<FormatSet>,
    ceiling_owed: bool,
    display: Option<ClientDisplay>,
    resize: ResizeGate,
    started: Instant,
    last_heard: Instant,
    next_keepalive: Instant,
}

impl ControlStage {
    pub(crate) fn new(
        network: &NetworkConfig,
        hints: HintPolicy,
        links: ControlLinks,
    ) -> Result<Self, HostError> {
        let now = Instant::now();
        let timing = ControlTiming::from(network);
        let resize = ResizeGate::new(
            links
                .virtual_output
                .as_ref()
                .map_or(Duration::ZERO, |output| {
                    Duration::from_millis(u64::from(output.limits.virtual_resize_min_interval_ms))
                }),
        );

        Ok(Self {
            timing,
            hints,
            takeover: SessionTakeover::new(links.peer, ReconnectTiming::from(network))?,
            links,
            streaming: false,
            unanswered_hello: None,
            ceiling_owed: false,
            display: None,
            resize,
            started: now,
            last_heard: now,
            next_keepalive: now + timing.keepalive_interval,
        })
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
                    if proves_peer_liveness(datagram) {
                        self.last_heard = now;
                    }
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
            self.announce_ceiling(now)?;
            self.apply_resize(now);
            self.keep_alive(now)?;
            self.service_clipboard(now);

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
        if refresh_peers(&self.links.peers).is_err() {
            if let Some(refusal) = busy_reply(datagram) {
                self.links.transport.send_to(&refusal, from).ok();
            }
            return Ok(());
        }

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
        self.resize.reset();

        events.clear();
        self.links.replay = replay;
        if let Some(link) = &mut self.links.clipboard {
            link.reset();
        }
        self.streaming = false;
        self.unanswered_hello = None;
        self.ceiling_owed = false;
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
            self.source_deadline().filter(|deadline| *deadline > now),
            self.resize.next_deadline(),
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
                self.forward_input(datagram);
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

    fn forward_input(&self, datagram: &[u8]) {
        if !self.streaming {
            return;
        }
        let Some(input) = &self.links.input else {
            return;
        };

        let forwarded = ForwardedDatagram {
            epoch: self.links.shared.client.epoch(),
            bytes: datagram.to_vec(),
        };
        if let Err(TrySendError::Full(_)) = input.try_send(forwarded) {
            self.links.shared.counters.input_packets_dropped.increment();
        }
    }

    fn forward_microphone(&self, datagram: &[u8]) {
        if !self.streaming {
            return;
        }
        let Some(microphone) = &self.links.microphone else {
            return;
        };

        let forwarded = ForwardedDatagram {
            epoch: self.links.shared.client.epoch(),
            bytes: datagram.to_vec(),
        };
        if let Err(TrySendError::Full(_)) = microphone.try_send(forwarded) {
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
                ControlEvent::Received(ControlMessage::Display(display)) => {
                    self.take_display(display);
                }
                ControlEvent::Received(ControlMessage::RequestKeyframe { frame }) => {
                    shared.mailbox.request_keyframe(frame);
                }
                ControlEvent::Received(ControlMessage::RequestRecovery { frame, last_good }) => {
                    shared
                        .mailbox
                        .request_recovery(RecoveryRequest { frame, last_good });
                }
                ControlEvent::Received(ControlMessage::AcknowledgeFrame { frame }) => {
                    shared.mailbox.acknowledge(frame);
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
                ControlEvent::Received(ControlMessage::Clipboard(chunk)) => {
                    self.receive_clipboard(&chunk);
                }
                ControlEvent::Received(ControlMessage::QualityRequest { enabled }) => {
                    shared.request_quality(enabled);
                }
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
                ControlEvent::Undeliverable {
                    message: ControlMessage::BitrateCeiling { .. },
                    ..
                } => {
                    shared.counters.control_undeliverable.increment();
                    self.ceiling_owed = true;
                }
                ControlEvent::Undeliverable { .. } => {
                    shared.counters.control_undeliverable.increment();
                }
                ControlEvent::Received(
                    ControlMessage::Keepalive
                    | ControlMessage::Rumble { .. }
                    | ControlMessage::Lightbar { .. }
                    | ControlMessage::PlayerLeds { .. }
                    | ControlMessage::TriggerEffect { .. }
                    | ControlMessage::StreamFormat(_)
                    | ControlMessage::BitrateCeiling { .. }
                    | ControlMessage::ClockPong { .. }
                    | ControlMessage::QualityReport(_)
                    | ControlMessage::QualityUnavailable { .. },
                )
                | ControlEvent::Acked { .. } => {}
            }
        }

        Ok(flow)
    }

    fn receive_clipboard(&mut self, chunk: &ClipboardChunk) {
        if !self.streaming {
            return;
        }
        let Some(link) = &mut self.links.clipboard else {
            return;
        };

        match link.receive(chunk) {
            Ok(()) => {}
            Err(ClipboardError::Protocol(_)) => {
                self.links.shared.counters.malformed_datagrams.increment();
            }
            Err(_) => self.links.clipboard = None,
        }
    }

    fn service_clipboard(&mut self, now: Instant) {
        if !self.streaming {
            return;
        }
        let Some(link) = &mut self.links.clipboard else {
            return;
        };

        let outbox = &self.links.outbox;
        let serviced = link.service(|chunk| {
            outbox
                .channel()
                .send(ControlMessage::Clipboard(chunk), now)
                .is_ok()
        });
        if serviced.is_err() {
            self.links.clipboard = None;
        }
    }

    fn answer_hello(&mut self, now: Instant) -> Result<(), HostError> {
        let shared = Arc::clone(&self.links.shared);
        let (Some(decodable), Some(encodable)) =
            (self.unanswered_hello, shared.negotiation.encodable())
        else {
            return Ok(());
        };
        if self.awaiting_display() || self.awaiting_source(now) {
            return Ok(());
        }
        self.unanswered_hello = None;

        self.send(
            ControlMessage::Hello {
                version: ProtocolVersion::CURRENT,
                formats: encodable,
            },
            now,
        )?;
        self.ceiling_owed = true;
        self.announce_ceiling(now)?;
        if self.streaming {
            return Ok(());
        }

        if let Some(agreed) = shared.negotiation.agreed() {
            return self.resume_after_takeover(agreed.chosen, encodable, decodable, now);
        }

        let source = self.links.source.source_traits();
        let preference = self
            .links
            .stream
            .preference(source.map(|traits| traits.depth));
        match negotiate(preference, encodable, decodable) {
            Ok(negotiated) => {
                if let Some((output, display)) =
                    self.links.virtual_output.as_ref().zip(self.display)
                {
                    shared.set_output(output.select(&display));
                } else if let Some((max_hertz, display)) =
                    self.links.follow_refresh.zip(self.display)
                {
                    shared.set_stream_rate(stream_rate(
                        &display,
                        max_hertz,
                        self.links.source.source_refresh(),
                    ));
                }
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

    fn take_display(&mut self, display: ClientDisplay) {
        let counters = &self.links.shared.counters;

        if self.links.shared.negotiation.agreed().is_none() {
            self.display = Some(display);
            return;
        }
        if self.links.virtual_output.is_none() {
            return;
        }

        counters.display_updates_received.increment();
        if self.resize.submit(display) {
            counters.display_updates_coalesced.increment();
        }
    }

    fn apply_resize(&mut self, now: Instant) {
        let Some(output) = &self.links.virtual_output else {
            return;
        };
        let Some(display) = self.resize.due(now) else {
            return;
        };

        let request = select_output(&display, &output.limits);
        if self.links.shared.output() == Some(request) {
            return;
        }

        output.offer(request);
        self.links.shared.set_output(request);
        self.links.shared.counters.virtual_resizes.increment();
    }

    fn awaiting_display(&self) -> bool {
        (self.links.virtual_output.is_some() || self.links.follow_refresh.is_some())
            && self.display.is_none()
            && self.links.shared.negotiation.agreed().is_none()
    }

    fn source_deadline(&self) -> Option<Instant> {
        (self.unanswered_hello.is_some()
            && self.links.stream.follows_source()
            && self.links.source.source_traits().is_none())
        .then_some(self.started + self.links.source_wait)
    }

    fn awaiting_source(&self, now: Instant) -> bool {
        self.source_deadline()
            .is_some_and(|deadline| now < deadline)
    }

    fn announce_ceiling(&mut self, now: Instant) -> Result<(), HostError> {
        if !self.ceiling_owed {
            return Ok(());
        }

        let ceiling = ControlMessage::BitrateCeiling {
            kbps: self.hints.ceiling_kbps(),
        };
        if self.send(ceiling, now)? {
            self.ceiling_owed = false;
        }
        Ok(())
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
    use rusplay_config::{RateControl, VaapiConfig, VideoEncoderConfig};
    use rusplay_net::{FecScheme, Packetizer};

    use super::*;

    fn policy(bitrate_kbps: u32, hint_min_kbps: u32) -> HintPolicy {
        let mut config = Config::default();
        config.video.bitrate_kbps = bitrate_kbps;
        config.network.bitrate_hint_min_kbps = hint_min_kbps;

        HintPolicy::from_config(&config)
    }

    #[test]
    fn the_configured_bitrate_is_the_ceiling_of_every_hint() {
        let hints = policy(40_000, 2_000);

        assert_eq!(hints, HintPolicy::Clamp(2_000..=40_000));
        assert_eq!(hints.ceiling_kbps(), 40_000);
        assert_eq!(hints.apply(60_000), Some(40_000));
        assert_eq!(hints.apply(30_000), Some(30_000));
        assert_eq!(hints.apply(1), Some(2_000));
    }

    #[test]
    fn a_hint_minimum_above_the_configured_bitrate_is_lowered_to_it() {
        let hints = policy(1_500, 2_000);

        assert_eq!(hints, HintPolicy::Clamp(1_500..=1_500));
        assert_eq!(hints.apply(1), Some(1_500));
        assert_eq!(hints.apply(u32::MAX), Some(1_500));
    }

    #[test]
    fn constant_quality_has_no_ceiling_to_announce() {
        let mut config = Config::default();
        config.video.encoder = VideoEncoderConfig::Vaapi(VaapiConfig {
            rate_control: RateControl::DEFAULT_CONSTANT_QUALITY,
            ..VaapiConfig::default()
        });

        let hints = HintPolicy::from_config(&config);

        assert_eq!(hints, HintPolicy::Ignore);
        assert_eq!(hints.ceiling_kbps(), 0);
        assert_eq!(hints.apply(12_345), None);
    }

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
