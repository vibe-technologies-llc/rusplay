use std::{
    collections::VecDeque,
    net::SocketAddr,
    time::{Duration, Instant},
};

use rusplay_config::NetworkConfig;
use rusplay_protocol::{ControlMessage, ControlPacket};

use crate::{NetError, Transport};

pub const CONTROL_WINDOW: u32 = 64;
const MAX_BACKOFF_SHIFT: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlConfig {
    pub retransmit_timeout: Duration,
    pub max_retransmits: u8,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self::from(&NetworkConfig::default())
    }
}

impl From<&NetworkConfig> for ControlConfig {
    fn from(config: &NetworkConfig) -> Self {
        Self {
            retransmit_timeout: Duration::from_millis(u64::from(config.control_retransmit_ms)),
            max_retransmits: config.control_max_retransmits,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlEvent {
    Received(ControlMessage),
    Acked { id: u32 },
    Undeliverable { id: u32, message: ControlMessage },
}

#[derive(Debug)]
struct InFlight {
    id: u32,
    message: ControlMessage,
    datagram: Vec<u8>,
    sent_at: Instant,
    deadline: Instant,
    retransmits: u8,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RttEstimator {
    smoothed: Option<Duration>,
    variation: Duration,
}

impl RttEstimator {
    fn observe(&mut self, sample: Duration) {
        match self.smoothed {
            None => {
                self.smoothed = Some(sample);
                self.variation = sample / 2;
            }
            Some(smoothed) => {
                self.variation = self.variation * 3 / 4 + smoothed.abs_diff(sample) / 4;
                self.smoothed = Some(smoothed * 7 / 8 + sample / 8);
            }
        }
    }
}

#[derive(Debug, Default)]
struct ReceiveWindow {
    highest: Option<u32>,
    seen: u64,
}

impl ReceiveWindow {
    fn insert(&mut self, id: u32) -> bool {
        let Some(highest) = self.highest else {
            self.highest = Some(id);
            self.seen = 1;
            return true;
        };

        let ahead = id.wrapping_sub(highest).cast_signed();
        if ahead > 0 {
            let shift = ahead.unsigned_abs();
            self.seen = self.seen.checked_shl(shift).unwrap_or(0) | 1;
            self.highest = Some(id);
            return true;
        }

        let behind = highest.wrapping_sub(id);
        if behind >= CONTROL_WINDOW {
            return false;
        }

        let bit = 1u64 << behind;
        let is_new = self.seen & bit == 0;
        self.seen |= bit;
        is_new
    }
}

#[derive(Debug)]
pub struct ControlChannel {
    config: ControlConfig,
    next_id: u32,
    in_flight: VecDeque<InFlight>,
    outbox: VecDeque<Vec<u8>>,
    received: ReceiveWindow,
    rtt: RttEstimator,
}

impl ControlChannel {
    pub fn new(config: ControlConfig) -> Self {
        Self {
            config,
            next_id: 0,
            in_flight: VecDeque::new(),
            outbox: VecDeque::new(),
            received: ReceiveWindow::default(),
            rtt: RttEstimator::default(),
        }
    }

    pub fn reset(&mut self) {
        self.next_id = 0;
        self.in_flight.clear();
        self.outbox.clear();
        self.received = ReceiveWindow::default();
        self.rtt = RttEstimator::default();
    }

    pub fn rtt(&self) -> Option<Duration> {
        self.rtt.smoothed
    }

    pub fn rtt_variation(&self) -> Option<Duration> {
        self.rtt.smoothed.map(|_| self.rtt.variation)
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    pub fn has_pending_transmit(&self) -> bool {
        !self.outbox.is_empty()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.in_flight.iter().map(|entry| entry.deadline).min()
    }

    pub fn send(&mut self, message: ControlMessage, now: Instant) -> Result<u32, NetError> {
        let window_full = self
            .in_flight
            .front()
            .is_some_and(|oldest| self.next_id.wrapping_sub(oldest.id) >= CONTROL_WINDOW);
        if window_full {
            return Err(NetError::ControlWindowFull {
                window: CONTROL_WINDOW,
            });
        }

        let id = self.next_id;
        let datagram = ControlPacket::Message { id, message }.to_datagram()?;

        self.next_id = self.next_id.wrapping_add(1);
        self.outbox.push_back(datagram.clone());
        self.in_flight.push_back(InFlight {
            id,
            message,
            datagram,
            sent_at: now,
            deadline: now + self.config.retransmit_timeout,
            retransmits: 0,
        });

        Ok(id)
    }

    pub fn handle_datagram(
        &mut self,
        datagram: &[u8],
        now: Instant,
        events: &mut Vec<ControlEvent>,
    ) -> Result<(), NetError> {
        match ControlPacket::read(datagram)? {
            ControlPacket::Ack { id } => {
                if let Some(position) = self.in_flight.iter().position(|entry| entry.id == id)
                    && let Some(acked) = self.in_flight.remove(position)
                {
                    if acked.retransmits == 0 {
                        self.rtt
                            .observe(now.saturating_duration_since(acked.sent_at));
                    }
                    events.push(ControlEvent::Acked { id });
                }
            }
            ControlPacket::Message { id, message } => {
                self.outbox
                    .push_back(ControlPacket::Ack { id }.to_datagram()?);

                if self.received.insert(id) {
                    events.push(ControlEvent::Received(message));
                }
            }
        }

        Ok(())
    }

    pub fn poll(&mut self, now: Instant, events: &mut Vec<ControlEvent>) {
        let mut index = 0;

        while index < self.in_flight.len() {
            let entry = &mut self.in_flight[index];

            if entry.deadline > now {
                index += 1;
                continue;
            }

            if entry.retransmits >= self.config.max_retransmits {
                if let Some(gone) = self.in_flight.remove(index) {
                    events.push(ControlEvent::Undeliverable {
                        id: gone.id,
                        message: gone.message,
                    });
                }
                continue;
            }

            entry.retransmits += 1;
            let backoff = u32::from(entry.retransmits).min(MAX_BACKOFF_SHIFT);
            entry.deadline = now + self.config.retransmit_timeout * (1 << backoff);
            self.outbox.push_back(entry.datagram.clone());
            index += 1;
        }
    }

    pub fn take_transmits(&mut self, out: &mut Vec<Vec<u8>>) {
        out.extend(self.outbox.drain(..));
    }

    pub fn flush(&mut self, transport: &Transport, peer: SocketAddr) -> Result<usize, NetError> {
        let mut sent = 0;

        while let Some(datagram) = self.outbox.pop_front() {
            if !transport.try_send_to(&datagram, peer)? {
                self.outbox.push_front(datagram);
                break;
            }
            sent += 1;
        }

        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use rusplay_protocol::ProtocolVersion;

    use super::*;

    const RTO: Duration = Duration::from_millis(50);

    fn config(max_retransmits: u8) -> ControlConfig {
        ControlConfig {
            retransmit_timeout: RTO,
            max_retransmits,
        }
    }

    fn channel(max_retransmits: u8) -> ControlChannel {
        ControlChannel::new(config(max_retransmits))
    }

    fn drain(channel: &mut ControlChannel) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        channel.take_transmits(&mut out);
        out
    }

    fn deliver(to: &mut ControlChannel, datagrams: &[Vec<u8>], events: &mut Vec<ControlEvent>) {
        for datagram in datagrams {
            to.handle_datagram(datagram, Instant::now(), events)
                .unwrap();
        }
    }

    fn keyframe(frame: u32) -> ControlMessage {
        ControlMessage::RequestKeyframe { frame }
    }

    #[test]
    fn message_is_delivered_and_acknowledged() {
        let mut a = channel(3);
        let mut b = channel(3);
        let now = Instant::now();
        let mut events_a = Vec::new();
        let mut events_b = Vec::new();

        let id = a.send(keyframe(9), now).unwrap();
        deliver(&mut b, &drain(&mut a), &mut events_b);
        deliver(&mut a, &drain(&mut b), &mut events_a);

        assert_eq!(events_b, [ControlEvent::Received(keyframe(9))]);
        assert_eq!(events_a, [ControlEvent::Acked { id }]);
        assert_eq!(a.in_flight(), 0);
        assert_eq!(a.next_deadline(), None);
    }

    #[test]
    fn lost_message_is_retransmitted_after_the_timeout() {
        let mut a = channel(3);
        let mut b = channel(3);
        let start = Instant::now();
        let mut events = Vec::new();

        a.send(keyframe(1), start).unwrap();
        drain(&mut a);

        a.poll(start + RTO / 2, &mut events);
        assert!(!a.has_pending_transmit());

        a.poll(start + RTO, &mut events);
        assert!(a.has_pending_transmit());
        deliver(&mut b, &drain(&mut a), &mut events);

        assert_eq!(events, [ControlEvent::Received(keyframe(1))]);
    }

    #[test]
    fn retransmit_interval_backs_off_exponentially() {
        let mut a = channel(8);
        let start = Instant::now();
        let mut events = Vec::new();
        a.send(ControlMessage::Goodbye, start).unwrap();

        let mut now = start;
        let mut gaps = Vec::new();
        for _ in 0..6 {
            let deadline = a.next_deadline().unwrap();
            gaps.push(deadline - now);
            now = deadline;
            a.poll(now, &mut events);
        }

        assert_eq!(gaps, [1, 2, 4, 8, 16, 16].map(|factor| RTO * factor));
    }

    #[test]
    fn lost_ack_causes_a_retransmit_that_is_not_delivered_twice() {
        let mut a = channel(3);
        let mut b = channel(3);
        let start = Instant::now();
        let mut events_a = Vec::new();
        let mut events_b = Vec::new();

        let id = a.send(keyframe(5), start).unwrap();
        deliver(&mut b, &drain(&mut a), &mut events_b);
        drain(&mut b);

        a.poll(start + RTO, &mut events_a);
        deliver(&mut b, &drain(&mut a), &mut events_b);
        deliver(&mut a, &drain(&mut b), &mut events_a);

        assert_eq!(events_b, [ControlEvent::Received(keyframe(5))]);
        assert_eq!(events_a, [ControlEvent::Acked { id }]);
    }

    #[test]
    fn message_is_given_up_after_the_retransmit_budget() {
        let mut a = channel(2);
        let start = Instant::now();
        let mut events = Vec::new();
        let id = a.send(keyframe(4), start).unwrap();

        while let Some(deadline) = a.next_deadline() {
            a.poll(deadline, &mut events);
        }

        assert_eq!(
            events,
            [ControlEvent::Undeliverable {
                id,
                message: keyframe(4)
            }]
        );
        assert_eq!(drain(&mut a).len(), 3);
    }

    #[test]
    fn zero_retransmits_gives_up_at_the_first_deadline() {
        let mut a = channel(0);
        let start = Instant::now();
        let mut events = Vec::new();
        a.send(ControlMessage::Goodbye, start).unwrap();

        a.poll(start + RTO, &mut events);

        assert!(matches!(events[..], [ControlEvent::Undeliverable { .. }]));
    }

    #[test]
    fn reordered_messages_are_each_delivered_once() {
        let mut a = channel(3);
        let mut b = channel(3);
        let now = Instant::now();
        let mut events = Vec::new();

        for frame in 0..5 {
            a.send(keyframe(frame), now).unwrap();
        }
        let mut datagrams = drain(&mut a);
        datagrams.reverse();
        let duplicate = datagrams[1].clone();
        datagrams.push(duplicate);
        deliver(&mut b, &datagrams, &mut events);

        let mut frames: Vec<u32> = events
            .iter()
            .map(|event| match event {
                ControlEvent::Received(ControlMessage::RequestKeyframe { frame }) => *frame,
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        frames.sort_unstable();
        assert_eq!(frames, [0, 1, 2, 3, 4]);
        assert_eq!(drain(&mut b).len(), 6);
    }

    #[test]
    fn window_limits_unacknowledged_messages() {
        let mut a = channel(3);
        let now = Instant::now();

        for frame in 0..CONTROL_WINDOW {
            a.send(keyframe(frame), now).unwrap();
        }

        assert!(matches!(
            a.send(keyframe(99), now),
            Err(NetError::ControlWindowFull {
                window: CONTROL_WINDOW
            })
        ));
    }

    #[test]
    fn acknowledgements_reopen_the_window() {
        let mut a = channel(3);
        let mut b = channel(3);
        let now = Instant::now();
        let mut events = Vec::new();

        for frame in 0..CONTROL_WINDOW {
            a.send(keyframe(frame), now).unwrap();
        }
        deliver(&mut b, &drain(&mut a), &mut events);
        deliver(&mut a, &drain(&mut b), &mut events);

        assert!(a.send(keyframe(99), now).is_ok());
    }

    #[test]
    fn a_stalled_oldest_message_blocks_new_sends_instead_of_aliasing() {
        let mut a = channel(3);
        let mut b = channel(3);
        let now = Instant::now();
        let mut events = Vec::new();

        a.send(keyframe(0), now).unwrap();
        let lost = drain(&mut a);
        for frame in 1..CONTROL_WINDOW {
            a.send(keyframe(frame), now).unwrap();
        }
        deliver(&mut b, &drain(&mut a), &mut events);
        deliver(&mut a, &drain(&mut b), &mut events);

        assert!(matches!(
            a.send(keyframe(100), now),
            Err(NetError::ControlWindowFull { .. })
        ));

        events.clear();
        deliver(&mut b, &lost, &mut events);
        assert_eq!(events, [ControlEvent::Received(keyframe(0))]);
    }

    #[test]
    fn ids_wrap_around_without_losing_messages() {
        let mut a = channel(3);
        let mut b = channel(3);
        let now = Instant::now();
        let mut events = Vec::new();
        a.next_id = u32::MAX - 2;

        for frame in 0..8 {
            a.send(keyframe(frame), now).unwrap();
            deliver(&mut b, &drain(&mut a), &mut events);
            deliver(&mut a, &drain(&mut b), &mut events);
        }

        let received = events
            .iter()
            .filter(|event| matches!(event, ControlEvent::Received(_)))
            .count();
        assert_eq!(received, 8);
        assert_eq!(a.in_flight(), 0);
    }

    #[test]
    fn stray_ack_is_ignored() {
        let mut a = channel(3);
        let mut events = Vec::new();
        let ack = ControlPacket::Ack { id: 12 }.to_datagram().unwrap();

        a.handle_datagram(&ack, Instant::now(), &mut events)
            .unwrap();

        assert_eq!(events.len(), 0);
    }

    #[test]
    fn malformed_datagram_is_an_error() {
        let mut a = channel(3);

        assert!(matches!(
            a.handle_datagram(&[1, 2, 3], Instant::now(), &mut Vec::new()),
            Err(NetError::Protocol(_))
        ));
    }

    #[test]
    fn reset_forgets_everything() {
        let mut a = channel(3);
        a.send(
            ControlMessage::Hello {
                version: ProtocolVersion::CURRENT,
                formats: rusplay_core::FormatSet::ALL,
            },
            Instant::now(),
        )
        .unwrap();

        a.reset();

        assert_eq!(a.in_flight(), 0);
        assert!(!a.has_pending_transmit());
        assert_eq!(a.send(ControlMessage::Goodbye, Instant::now()).unwrap(), 0);
    }

    fn round_trip(a: &mut ControlChannel, b: &mut ControlChannel, sent: Instant, rtt: Duration) {
        let mut events = Vec::new();
        a.send(ControlMessage::Keepalive, sent).unwrap();
        for datagram in drain(a) {
            b.handle_datagram(&datagram, sent, &mut events).unwrap();
        }
        for datagram in drain(b) {
            a.handle_datagram(&datagram, sent + rtt, &mut events)
                .unwrap();
        }
    }

    #[test]
    fn rtt_is_unknown_before_any_ack() {
        let mut a = channel(3);
        a.send(ControlMessage::Keepalive, Instant::now()).unwrap();

        assert_eq!(a.rtt(), None);
        assert_eq!(a.rtt_variation(), None);
    }

    #[test]
    fn first_ack_seeds_the_estimate() {
        let mut a = channel(3);
        let mut b = channel(3);

        round_trip(&mut a, &mut b, Instant::now(), Duration::from_millis(8));

        assert_eq!(a.rtt(), Some(Duration::from_millis(8)));
        assert_eq!(a.rtt_variation(), Some(Duration::from_millis(4)));
    }

    #[test]
    fn later_acks_are_smoothed_per_rfc_6298() {
        let mut a = channel(3);
        let mut b = channel(3);
        let start = Instant::now();

        round_trip(&mut a, &mut b, start, Duration::from_millis(8));
        round_trip(&mut a, &mut b, start, Duration::from_millis(16));

        assert_eq!(a.rtt(), Some(Duration::from_millis(9)));
        assert_eq!(a.rtt_variation(), Some(Duration::from_millis(5)));
    }

    #[test]
    fn retransmitted_messages_do_not_sample_the_rtt() {
        let mut a = channel(3);
        let mut b = channel(3);
        let start = Instant::now();
        let mut events = Vec::new();

        a.send(ControlMessage::Keepalive, start).unwrap();
        drain(&mut a);
        a.poll(start + RTO, &mut events);
        for datagram in drain(&mut a) {
            b.handle_datagram(&datagram, start + RTO, &mut events)
                .unwrap();
        }
        for datagram in drain(&mut b) {
            a.handle_datagram(&datagram, start + RTO * 2, &mut events)
                .unwrap();
        }

        assert_eq!(a.in_flight(), 0);
        assert_eq!(a.rtt(), None);
    }

    #[test]
    fn duplicate_acks_sample_once() {
        let mut a = channel(3);
        let mut b = channel(3);
        let start = Instant::now();
        let mut events = Vec::new();

        a.send(ControlMessage::Keepalive, start).unwrap();
        for datagram in drain(&mut a) {
            b.handle_datagram(&datagram, start, &mut events).unwrap();
        }
        let acks = drain(&mut b);
        a.handle_datagram(&acks[0], start + Duration::from_millis(5), &mut events)
            .unwrap();
        a.handle_datagram(&acks[0], start + Duration::from_millis(50), &mut events)
            .unwrap();

        assert_eq!(a.rtt(), Some(Duration::from_millis(5)));
    }

    #[test]
    fn reset_forgets_the_rtt() {
        let mut a = channel(3);
        let mut b = channel(3);

        round_trip(&mut a, &mut b, Instant::now(), Duration::from_millis(8));
        a.reset();

        assert_eq!(a.rtt(), None);
    }

    #[test]
    fn config_follows_the_network_config() {
        let network = NetworkConfig {
            control_retransmit_ms: 25,
            control_max_retransmits: 2,
            ..NetworkConfig::default()
        };

        assert_eq!(
            ControlConfig::from(&network),
            ControlConfig {
                retransmit_timeout: Duration::from_millis(25),
                max_retransmits: 2,
            }
        );
    }
}
