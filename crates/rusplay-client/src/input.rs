use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    time::{Duration, Instant},
};

use rusplay_input::{
    GamepadEvent, HeldInput, InputEvent, InputRecord, RumbleCommand, StateRecord,
    capture::EventCoalescer,
    gamepad::{PhysicalGamepads, PhysicalOptions},
};
use rusplay_net::Transport;
use rusplay_protocol::{InputPacket, push_input_record};

use crate::{ClientError, stats::Counters};

const IDLE_WAKE: Duration = Duration::from_millis(10);
const GAMEPAD_POLL: Duration = Duration::from_millis(5);
const MAX_RECORD_LEN: usize = StateRecord::MAX_ENCODED_LEN + 1;

#[derive(Debug, Clone)]
pub struct InputHandle {
    sender: Sender<InputEvent>,
}

impl InputHandle {
    pub(crate) fn new(sender: Sender<InputEvent>) -> Self {
        Self { sender }
    }

    pub fn send(&self, event: InputEvent) -> bool {
        self.sender.send(event).is_ok()
    }

    pub fn send_all(&self, events: impl IntoIterator<Item = InputEvent>) -> bool {
        events.into_iter().all(|event| self.send(event))
    }
}

#[derive(Debug)]
pub(crate) struct InputPacketer {
    sequence: u32,
    section_limit: usize,
    previous: Vec<u8>,
    tail_pending: bool,
}

impl InputPacketer {
    pub(crate) fn new(datagram_budget: usize) -> Self {
        let sections = datagram_budget.saturating_sub(InputPacket::OVERHEAD);

        Self {
            sequence: 0,
            section_limit: (sections / 2).max(MAX_RECORD_LEN),
            previous: Vec::new(),
            tail_pending: false,
        }
    }

    pub(crate) fn needs_repeat(&self) -> bool {
        self.tail_pending
    }

    pub(crate) fn pack(
        &mut self,
        events: &[InputEvent],
        out: &mut Vec<Vec<u8>>,
    ) -> Result<(), ClientError> {
        self.pack_records(events.iter().copied().map(InputRecord::Event), out)
    }

    pub(crate) fn pack_state(
        &mut self,
        records: &[StateRecord],
        out: &mut Vec<Vec<u8>>,
    ) -> Result<(), ClientError> {
        self.pack_records(records.iter().copied().map(InputRecord::State), out)
    }

    fn pack_records(
        &mut self,
        records: impl Iterator<Item = InputRecord>,
        out: &mut Vec<Vec<u8>>,
    ) -> Result<(), ClientError> {
        let mut current = Vec::new();
        let mut encoded = Vec::new();
        let mut stateful = false;

        for record in records {
            encoded.clear();
            record.encode_into(&mut encoded);

            if !current.is_empty() && current.len() + 1 + encoded.len() > self.section_limit {
                self.emit(std::mem::take(&mut current), stateful, out)?;
                stateful = false;
            }

            push_input_record(&mut current, &encoded)?;
            stateful |= matches!(record, InputRecord::Event(event) if changes_state(&event));
        }

        if current.is_empty() {
            Ok(())
        } else {
            self.emit(current, stateful, out)
        }
    }

    pub(crate) fn repeat(&mut self, out: &mut Vec<Vec<u8>>) -> Result<(), ClientError> {
        let mut datagram = Vec::new();
        InputPacket {
            sequence: self.sequence,
            current: &[],
            previous: &self.previous,
        }
        .write(&mut datagram)?;

        out.push(datagram);
        self.sequence = self.sequence.wrapping_add(1);
        self.tail_pending = false;

        Ok(())
    }

    fn emit(
        &mut self,
        current: Vec<u8>,
        stateful: bool,
        out: &mut Vec<Vec<u8>>,
    ) -> Result<(), ClientError> {
        let mut datagram = Vec::new();
        InputPacket {
            sequence: self.sequence,
            current: &current,
            previous: &self.previous,
        }
        .write(&mut datagram)?;

        out.push(datagram);
        self.sequence = self.sequence.wrapping_add(1);
        self.previous = current;
        self.tail_pending = stateful;

        Ok(())
    }
}

fn changes_state(event: &InputEvent) -> bool {
    match event {
        InputEvent::Key { .. } | InputEvent::PointerButton { .. } => true,
        InputEvent::Gamepad { event, .. } => !matches!(event, GamepadEvent::Axis { .. }),
        InputEvent::PointerMotion { .. }
        | InputEvent::PointerAbsolute { .. }
        | InputEvent::Scroll { .. } => false,
    }
}

#[derive(Debug)]
pub(crate) struct InputLinks {
    pub events: Receiver<InputEvent>,
    pub gamepads_done: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InputTiming {
    pub resend_delay: Duration,
    pub snapshot_interval: Duration,
}

pub(crate) struct InputStage {
    transport: Arc<Transport>,
    host: SocketAddr,
    timing: InputTiming,
    links: InputLinks,
}

impl InputStage {
    pub(crate) fn new(
        transport: Arc<Transport>,
        host: SocketAddr,
        timing: InputTiming,
        links: InputLinks,
    ) -> Self {
        Self {
            transport,
            host,
            timing,
            links,
        }
    }

    pub(crate) fn run(self) -> Result<(), ClientError> {
        let result = self.serve();
        self.links.stop.store(true, Ordering::Relaxed);

        result
    }

    fn serve(&self) -> Result<(), ClientError> {
        let mut packeter = InputPacketer::new(self.transport.max_datagram_len(&self.host));
        let mut coalescer = EventCoalescer::new();
        let mut events = Vec::new();
        let mut datagrams = Vec::new();
        let mut held = HeldInput::new();
        let mut records = Vec::new();
        let mut repeat_at: Option<Instant> = None;
        let mut next_snapshot = self.next_snapshot(Instant::now());

        loop {
            let finishing = self.links.stop.load(Ordering::Relaxed)
                && self.links.gamepads_done.load(Ordering::Relaxed);
            let wait = repeat_at
                .map_or(IDLE_WAKE, |due| {
                    due.saturating_duration_since(Instant::now())
                })
                .min(IDLE_WAKE);

            let disconnected = match self.links.events.recv_timeout(wait) {
                Ok(event) => {
                    coalescer.push(event);
                    false
                }
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => true,
            };
            while let Ok(event) = self.links.events.try_recv() {
                coalescer.push(event);
            }

            if !coalescer.is_empty() {
                events.clear();
                coalescer.drain_into(&mut events);
                for event in &events {
                    held.observe(event);
                }
                packeter.pack(&events, &mut datagrams)?;
                self.links
                    .counters
                    .input_events_sent
                    .add(u64::try_from(events.len()).unwrap_or(u64::MAX));
                repeat_at = (packeter.needs_repeat() && !self.timing.resend_delay.is_zero())
                    .then(|| Instant::now() + self.timing.resend_delay);
            } else if repeat_at.is_some_and(|due| Instant::now() >= due) {
                packeter.repeat(&mut datagrams)?;
                repeat_at = None;
            }

            let now = Instant::now();
            if next_snapshot.is_some_and(|due| now >= due) {
                records.clear();
                held.snapshot(&mut records);
                packeter.pack_state(&records, &mut datagrams)?;
                self.links.counters.input_snapshots_sent.increment();
                next_snapshot = self.next_snapshot(now);
                if !packeter.needs_repeat() {
                    repeat_at = None;
                }
            }

            self.send(&mut datagrams)?;

            if finishing || disconnected {
                return Ok(());
            }
        }
    }

    fn next_snapshot(&self, from: Instant) -> Option<Instant> {
        (!self.timing.snapshot_interval.is_zero()).then(|| from + self.timing.snapshot_interval)
    }

    fn send(&self, datagrams: &mut Vec<Vec<u8>>) -> Result<(), ClientError> {
        for datagram in datagrams.drain(..) {
            self.transport.send_to(&datagram, self.host)?;
            self.links.counters.input_packets_sent.increment();
        }

        Ok(())
    }
}

#[derive(Debug)]
struct DoneOnDrop(Arc<AtomicBool>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub(crate) struct GamepadLinks {
    pub rumble: Receiver<RumbleCommand>,
    pub input: InputHandle,
    pub done: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
}

pub(crate) fn run_gamepads(options: PhysicalOptions, links: &GamepadLinks) {
    let _done = DoneOnDrop(Arc::clone(&links.done));
    let mut pads = PhysicalGamepads::new(options);
    let mut events = Vec::new();

    while !links.stop.load(Ordering::Relaxed) {
        while let Ok(command) = links.rumble.try_recv() {
            links.counters.rumble_commands.increment();
            if pads.rumble(&command).is_err() {
                links.counters.rumble_failures.increment();
            }
        }

        if pads.poll(GAMEPAD_POLL, &mut events).is_err() {
            links.counters.gamepad_failures.increment();
            break;
        }

        links.input.send_all(events.drain(..));
    }

    pads.disconnect_all(&mut events);
    links.input.send_all(events.drain(..));
}

#[cfg(test)]
mod tests {
    use rusplay_input::{GamepadAxis, GamepadButton};
    use rusplay_protocol::{InputAdmission, InputSequence, PacketHeader, StreamKind};

    use super::*;

    const BUDGET: usize = 1200;
    const SMALL_BUDGET: usize = 300;

    fn key(keycode: u32, pressed: bool) -> InputEvent {
        InputEvent::Key { keycode, pressed }
    }

    fn motion(dx: f64) -> InputEvent {
        InputEvent::PointerMotion { dx, dy: 0.0 }
    }

    fn decode(datagram: &[u8]) -> (u32, Vec<InputEvent>, Vec<InputEvent>) {
        let packet = InputPacket::read(datagram).unwrap();
        let events = |records: rusplay_protocol::InputRecords<'_>| {
            records
                .map(|record| InputEvent::decode(record.unwrap()).unwrap())
                .collect()
        };

        (
            packet.sequence,
            events(packet.current_records()),
            events(packet.previous_records()),
        )
    }

    fn received(datagrams: &[Vec<u8>], lose: &[usize]) -> Vec<InputEvent> {
        let mut sequence = InputSequence::new();
        let mut applied = Vec::new();

        for (index, datagram) in datagrams.iter().enumerate() {
            if lose.contains(&index) {
                continue;
            }
            let (number, current, previous) = decode(datagram);
            match sequence.admit(number) {
                InputAdmission::Stale => {}
                InputAdmission::Current => applied.extend(current),
                InputAdmission::PreviousAndCurrent => {
                    applied.extend(previous);
                    applied.extend(current);
                }
            }
        }

        applied
    }

    #[test]
    fn events_become_one_datagram_with_a_running_sequence() {
        let mut packeter = InputPacketer::new(BUDGET);
        let mut out = Vec::new();

        packeter
            .pack(&[key(30, true), motion(2.0)], &mut out)
            .unwrap();
        packeter.pack(&[key(30, false)], &mut out).unwrap();

        assert_eq!(out.len(), 2);
        assert_eq!(
            decode(&out[0]),
            (0, vec![key(30, true), motion(2.0)], Vec::new())
        );
        assert_eq!(
            decode(&out[1]),
            (1, vec![key(30, false)], vec![key(30, true), motion(2.0)])
        );
        assert_eq!(
            PacketHeader::parse(&out[0]).unwrap().0.stream,
            StreamKind::Input
        );
    }

    #[test]
    fn a_long_batch_is_split_into_datagrams_within_budget() {
        let mut packeter = InputPacketer::new(SMALL_BUDGET);
        let mut out = Vec::new();
        let events: Vec<_> = (0..60).map(|code| key(code, true)).collect();

        packeter.pack(&events, &mut out).unwrap();

        assert!(out.len() > 1);
        assert!(out.iter().all(|datagram| datagram.len() <= SMALL_BUDGET));
        assert_eq!(received(&out, &[]), events);
    }

    #[test]
    fn a_lost_datagram_is_recovered_from_the_next_one() {
        let mut packeter = InputPacketer::new(BUDGET);
        let mut out = Vec::new();
        for code in 1..=4 {
            packeter.pack(&[key(code, true)], &mut out).unwrap();
        }

        let applied = received(&out, &[1]);

        assert_eq!(
            applied,
            (1..=4).map(|code| key(code, true)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_tail_repeat_recovers_the_final_datagram() {
        let mut packeter = InputPacketer::new(BUDGET);
        let mut out = Vec::new();
        packeter.pack(&[key(30, true)], &mut out).unwrap();
        packeter.pack(&[key(30, false)], &mut out).unwrap();

        assert!(packeter.needs_repeat());
        packeter.repeat(&mut out).unwrap();

        assert!(!packeter.needs_repeat());
        assert_eq!(received(&out, &[1]), vec![key(30, true), key(30, false)]);
        assert_eq!(received(&out, &[]), vec![key(30, true), key(30, false)]);
    }

    #[test]
    fn motion_alone_never_asks_for_a_repeat() {
        let mut packeter = InputPacketer::new(BUDGET);
        let mut out = Vec::new();

        packeter.pack(&[motion(1.0)], &mut out).unwrap();

        assert!(!packeter.needs_repeat());
    }

    #[test]
    fn only_state_changes_ask_for_a_repeat() {
        let axis = InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Axis {
                axis: GamepadAxis::LEFT_X,
                value: 0.5,
            },
        };
        let press = InputEvent::Gamepad {
            slot: 0,
            event: GamepadEvent::Button {
                button: GamepadButton::SOUTH,
                pressed: true,
            },
        };
        let connected = InputEvent::Gamepad {
            slot: 1,
            event: GamepadEvent::Connected,
        };

        assert!(!changes_state(&axis));
        assert!(changes_state(&press));
        assert!(changes_state(&connected));
        assert!(changes_state(&key(1, false)));
        assert!(!changes_state(&InputEvent::Scroll { dx: 0.0, dy: 1.0 }));
    }

    fn decode_records(datagram: &[u8]) -> (u32, Vec<InputRecord>, Vec<InputRecord>) {
        let packet = InputPacket::read(datagram).unwrap();
        let records = |records: rusplay_protocol::InputRecords<'_>| {
            records
                .map(|record| InputRecord::decode(record.unwrap()).unwrap())
                .collect()
        };

        (
            packet.sequence,
            records(packet.current_records()),
            records(packet.previous_records()),
        )
    }

    fn host_view(datagrams: &[Vec<u8>], lose: &[usize]) -> HeldInput {
        let mut sequence = InputSequence::new();
        let mut host = HeldInput::new();

        for (index, datagram) in datagrams.iter().enumerate() {
            if lose.contains(&index) {
                continue;
            }
            let (number, current, previous) = decode_records(datagram);
            let records: Vec<_> = match sequence.admit(number) {
                InputAdmission::Stale => Vec::new(),
                InputAdmission::Current => current,
                InputAdmission::PreviousAndCurrent => previous.into_iter().chain(current).collect(),
            };

            for record in records {
                let mut events = Vec::new();
                match record {
                    InputRecord::Event(event) => events.push(event),
                    InputRecord::State(state) => host.reconcile(&state, &mut events),
                }
                for event in &events {
                    host.observe(event);
                }
            }
        }

        host
    }

    fn snapshot_of(client: &HeldInput, packeter: &mut InputPacketer, out: &mut Vec<Vec<u8>>) {
        let mut records = Vec::new();
        client.snapshot(&mut records);
        packeter.pack_state(&records, out).unwrap();
    }

    #[test]
    fn a_snapshot_is_one_datagram_of_state_records() {
        let mut packeter = InputPacketer::new(BUDGET);
        let mut client = HeldInput::new();
        client.observe(&key(30, true));
        let mut out = Vec::new();

        snapshot_of(&client, &mut packeter, &mut out);

        let (sequence, current, previous) = decode_records(&out[0]);
        let mut expected = Vec::new();
        client.snapshot(&mut expected);
        assert_eq!(out.len(), 1);
        assert_eq!(sequence, 0);
        assert_eq!(
            current,
            expected
                .into_iter()
                .map(InputRecord::State)
                .collect::<Vec<_>>()
        );
        assert_eq!(previous, Vec::new());
        assert!(!packeter.needs_repeat());
    }

    #[test]
    fn a_snapshot_repairs_a_release_lost_with_its_repeat() {
        let mut packeter = InputPacketer::new(BUDGET);
        let mut client = HeldInput::new();
        let mut out = Vec::new();
        for events in [vec![key(30, true)], vec![key(30, false)], vec![motion(1.0)]] {
            for event in &events {
                client.observe(event);
            }
            packeter.pack(&events, &mut out).unwrap();
        }
        snapshot_of(&client, &mut packeter, &mut out);

        let without_snapshot = host_view(&out[..3], &[1, 2]);
        let with_snapshot = host_view(&out, &[1, 2]);

        assert!(without_snapshot.keys().contains(30));
        assert!(with_snapshot.is_idle());
        assert_eq!(with_snapshot, client);
    }

    #[test]
    fn a_lost_snapshot_leaves_the_host_as_it_was() {
        let mut packeter = InputPacketer::new(BUDGET);
        let mut client = HeldInput::new();
        let mut out = Vec::new();
        client.observe(&key(30, true));
        packeter.pack(&[key(30, true)], &mut out).unwrap();
        snapshot_of(&client, &mut packeter, &mut out);

        let host = host_view(&out, &[1]);

        assert!(host.keys().contains(30));
    }

    #[test]
    fn many_pads_split_a_snapshot_across_datagrams_within_budget() {
        let mut packeter = InputPacketer::new(SMALL_BUDGET);
        let mut client = HeldInput::new();
        for slot in 0..8 {
            client.observe(&InputEvent::Gamepad {
                slot,
                event: GamepadEvent::Connected,
            });
            client.observe(&InputEvent::Gamepad {
                slot,
                event: GamepadEvent::Axis {
                    axis: GamepadAxis::LEFT_X,
                    value: 0.5,
                },
            });
        }
        let mut out = Vec::new();

        snapshot_of(&client, &mut packeter, &mut out);

        assert!(out.len() > 1);
        assert!(out.iter().all(|datagram| datagram.len() <= SMALL_BUDGET));
        assert_eq!(host_view(&out, &[]), client);
    }

    #[test]
    fn nothing_to_pack_sends_nothing() {
        let mut packeter = InputPacketer::new(BUDGET);
        let mut out = Vec::new();

        packeter.pack(&[], &mut out).unwrap();

        assert_eq!(out, Vec::<Vec<u8>>::new());
    }
}
