use std::{
    fmt,
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use parking_lot::{Mutex, MutexGuard};
use rusplay_input::{HeldInput, InputError, InputEvent, InputRecord, PadFeedback, StateRecord};
use rusplay_net::NetError;
use rusplay_protocol::{ControlMessage, InputAdmission, InputPacket, InputRecords, InputSequence};

use crate::{
    HostError, Stage, datagrams::ForwardedDatagram, outbox::ControlOutbox, shared::Shared,
};

const SERVICE_INTERVAL: Duration = Duration::from_millis(4);
const IDLE_SERVICE_INTERVAL: Duration = Duration::from_millis(50);
const INJECT_TIMEOUT: Duration = Duration::from_millis(250);
const INJECTOR_THREAD: &str = "rusplay-host-injector";

pub trait InputTarget {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;

    fn inject_batch(&mut self, events: &[InputEvent], results: &mut Vec<Result<(), InputError>>) {
        results.clear();
        results.extend(events.iter().map(|event| self.inject(*event)));
    }

    fn service(&mut self, feedback: &mut Vec<PadFeedback>) -> Result<(), InputError>;
}

pub struct InputFeed {
    target: Mutex<Box<dyn InputTarget + Send>>,
}

impl InputFeed {
    pub fn new(target: impl InputTarget + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(Box::new(target)),
        })
    }

    pub fn spawn<T, F>(build: F) -> Result<Arc<Self>, HostError>
    where
        T: InputTarget + 'static,
        F: FnOnce() -> Result<T, HostError> + Send + 'static,
    {
        let (requests, request_queue) = mpsc::channel();
        let (reply_sender, replies) = mpsc::channel();
        let (ready_sender, ready) = mpsc::channel();

        thread::Builder::new()
            .name(INJECTOR_THREAD.to_owned())
            .spawn(move || match build() {
                Ok(target) => {
                    ready_sender.send(Ok(())).ok();
                    serve_requests(target, &request_queue, &reply_sender);
                }
                Err(error) => {
                    ready_sender.send(Err(error)).ok();
                }
            })
            .map_err(|source| HostError::SpawnStage {
                stage: Stage::Input,
                source,
            })?;

        match ready.recv() {
            Ok(Ok(())) => Ok(Self::new(RemoteTarget {
                requests,
                replies,
                next_id: 0,
            })),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(HostError::StagePanicked {
                stage: Stage::Input,
            }),
        }
    }

    pub(crate) fn target(&self) -> MutexGuard<'_, Box<dyn InputTarget + Send>> {
        self.target.lock()
    }
}

enum Request {
    Inject(Vec<InputEvent>),
    Service,
}

enum Reply {
    Injected(Vec<Result<(), InputError>>),
    Serviced {
        feedback: Vec<PadFeedback>,
        result: Result<(), InputError>,
    },
}

struct RemoteTarget {
    requests: Sender<(u64, Request)>,
    replies: Receiver<(u64, Reply)>,
    next_id: u64,
}

impl RemoteTarget {
    fn call(&mut self, request: Request) -> Result<Reply, InputError> {
        self.next_id += 1;
        let id = self.next_id;
        self.requests
            .send((id, request))
            .map_err(|_| InputError::SinkClosed)?;

        let deadline = Instant::now() + INJECT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(remaining) {
                Ok((answered, reply)) if answered == id => return Ok(reply),
                Ok(_) => {}
                Err(_) => return Err(InputError::SinkClosed),
            }
        }
    }
}

impl InputTarget for RemoteTarget {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        let mut results = Vec::with_capacity(1);
        self.inject_batch(&[event], &mut results);

        results.pop().unwrap_or(Err(InputError::SinkClosed))
    }

    fn inject_batch(&mut self, events: &[InputEvent], results: &mut Vec<Result<(), InputError>>) {
        results.clear();
        match self.call(Request::Inject(events.to_vec())) {
            Ok(Reply::Injected(injected)) => results.extend(injected),
            Ok(Reply::Serviced { .. }) | Err(_) => {
                results.extend(events.iter().map(|_| Err(InputError::SinkClosed)));
            }
        }
    }

    fn service(&mut self, feedback: &mut Vec<PadFeedback>) -> Result<(), InputError> {
        match self.call(Request::Service)? {
            Reply::Serviced {
                feedback: commands,
                result,
            } => {
                feedback.extend(commands);
                result
            }
            Reply::Injected(_) => Err(InputError::SinkClosed),
        }
    }
}

fn control_message(feedback: PadFeedback) -> ControlMessage {
    match feedback {
        PadFeedback::Rumble(command) => ControlMessage::Rumble {
            slot: command.slot,
            strong: command.strong,
            weak: command.weak,
            delay_ms: command.delay_ms,
            duration_ms: command.duration_ms,
        },
        PadFeedback::Lightbar {
            slot,
            red,
            green,
            blue,
        } => ControlMessage::Lightbar {
            slot,
            red,
            green,
            blue,
        },
        PadFeedback::PlayerLeds { slot, mask } => ControlMessage::PlayerLeds { slot, mask },
        PadFeedback::TriggerEffect {
            slot,
            right,
            effect,
        } => ControlMessage::TriggerEffect {
            slot,
            right,
            effect,
        },
    }
}

fn serve_requests(
    mut target: impl InputTarget,
    requests: &Receiver<(u64, Request)>,
    replies: &Sender<(u64, Reply)>,
) {
    let mut idle = Vec::new();

    loop {
        match requests.recv_timeout(IDLE_SERVICE_INTERVAL) {
            Ok((id, Request::Inject(events))) => {
                let mut results = Vec::with_capacity(events.len());
                target.inject_batch(&events, &mut results);
                replies.send((id, Reply::Injected(results))).ok();
            }
            Ok((id, Request::Service)) => {
                let mut feedback = Vec::new();
                let result = target.service(&mut feedback);
                replies
                    .send((id, Reply::Serviced { feedback, result }))
                    .ok();
            }
            Err(RecvTimeoutError::Timeout) => {
                idle.clear();
                target.service(&mut idle).ok();
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

impl fmt::Debug for InputFeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("InputFeed").finish_non_exhaustive()
    }
}

pub(crate) struct InputLinks {
    pub shared: Arc<Shared>,
    pub feed: Arc<InputFeed>,
    pub datagrams: Receiver<ForwardedDatagram>,
    pub outbox: Arc<ControlOutbox>,
}

pub(crate) fn run(links: &InputLinks) -> Result<(), HostError> {
    let mut guard = links.feed.target();
    let target: &mut dyn InputTarget = &mut **guard;
    let mut stage = InputStage::new(&links.shared, &links.outbox);

    let result = stage.serve(target, &links.datagrams);
    stage.release_all(target);
    links.shared.stop();

    result
}

struct InputStage<'a> {
    shared: &'a Shared,
    outbox: &'a ControlOutbox,
    sequence: InputSequence,
    epoch: u32,
    held: HeldInput,
    pending: Vec<InputEvent>,
    repairs: Vec<InputEvent>,
    outcomes: Vec<Result<(), InputError>>,
    commands: Vec<PadFeedback>,
    access_reported: bool,
}

impl<'a> InputStage<'a> {
    fn new(shared: &'a Shared, outbox: &'a ControlOutbox) -> Self {
        Self {
            shared,
            outbox,
            sequence: InputSequence::new(),
            epoch: shared.client.epoch(),
            held: HeldInput::new(),
            pending: Vec::new(),
            repairs: Vec::new(),
            outcomes: Vec::new(),
            commands: Vec::new(),
            access_reported: false,
        }
    }

    fn serve(
        &mut self,
        target: &mut dyn InputTarget,
        datagrams: &Receiver<ForwardedDatagram>,
    ) -> Result<(), HostError> {
        while !self.shared.stopped() {
            self.follow_client(target);

            match datagrams.recv_timeout(SERVICE_INTERVAL) {
                Ok(datagram) => {
                    self.admit(target, &datagram);
                    while let Ok(next) = datagrams.try_recv() {
                        self.admit(target, &next);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }

            self.service(target)?;
        }

        Ok(())
    }

    fn follow_client(&mut self, target: &mut dyn InputTarget) {
        let epoch = self.shared.client.epoch();
        if epoch == self.epoch {
            return;
        }

        self.release_all(target);
        self.held = HeldInput::new();
        self.sequence.reset();
        self.epoch = epoch;
    }

    fn admit(&mut self, target: &mut dyn InputTarget, datagram: &ForwardedDatagram) {
        self.follow_client(target);

        if datagram.epoch == self.epoch {
            self.apply_datagram(target, &datagram.bytes);
        } else {
            self.shared.counters.input_packets_stale.increment();
        }
    }

    fn apply_datagram(&mut self, target: &mut dyn InputTarget, datagram: &[u8]) {
        let counters = &self.shared.counters;
        let Ok(packet) = InputPacket::read(datagram) else {
            counters.malformed_datagrams.increment();
            return;
        };
        counters.input_packets.increment();

        match self.sequence.admit(packet.sequence) {
            InputAdmission::Stale => counters.input_packets_stale.increment(),
            InputAdmission::Current => self.apply_section(target, packet.current_records()),
            InputAdmission::PreviousAndCurrent => {
                self.apply_section(target, packet.previous_records());
                self.apply_section(target, packet.current_records());
            }
        }
    }

    fn apply_section(&mut self, target: &mut dyn InputTarget, records: InputRecords<'_>) {
        let mut events = std::mem::take(&mut self.pending);
        events.clear();

        for bytes in records {
            let decoded = bytes.ok().and_then(|bytes| InputRecord::decode(bytes).ok());

            match decoded {
                Some(InputRecord::Event(event)) => events.push(event),
                Some(InputRecord::State(state)) => {
                    self.inject_all(target, &events);
                    events.clear();
                    self.reconcile(target, &state);
                }
                None => self.shared.counters.malformed_datagrams.increment(),
            }
        }

        self.inject_all(target, &events);
        events.clear();
        self.pending = events;
    }

    fn reconcile(&mut self, target: &mut dyn InputTarget, state: &StateRecord) {
        self.shared.counters.input_state_records.increment();

        let mut events = std::mem::take(&mut self.repairs);
        events.clear();
        self.held.reconcile(state, &mut events);
        let repaired = self.inject_all(target, &events);
        self.shared
            .counters
            .input_events_repaired
            .add(repaired as u64);
        self.repairs = events;
    }

    fn inject_all(&mut self, target: &mut dyn InputTarget, events: &[InputEvent]) -> usize {
        if events.is_empty() {
            return 0;
        }

        let counters = &self.shared.counters;
        let mut injected = 0;
        target.inject_batch(events, &mut self.outcomes);

        for (event, outcome) in events.iter().zip(&self.outcomes) {
            match outcome {
                Ok(()) => {
                    self.held.observe(event);
                    counters.input_events.increment();
                    injected += 1;
                }
                Err(error) if error.is_device_access_denied() && !self.access_reported => {
                    self.access_reported = true;
                    eprintln!("rusplay-host: input: {error}");
                }
                Err(_) => {}
            }
        }
        let failed = events.len() - injected;
        counters.input_failures.add(failed as u64);

        injected
    }

    fn service(&mut self, target: &mut dyn InputTarget) -> Result<(), HostError> {
        let counters = &self.shared.counters;

        self.commands.clear();
        if target.service(&mut self.commands).is_err() {
            counters.input_failures.increment();
        }
        if self.commands.is_empty() {
            return Ok(());
        }

        for feedback in self.commands.drain(..) {
            let rumble = matches!(feedback, PadFeedback::Rumble(_));
            let sent = self
                .outbox
                .channel()
                .send(control_message(feedback), Instant::now());
            let (delivered, dropped) = if rumble {
                (&counters.rumble_sent, &counters.rumble_dropped)
            } else {
                (&counters.feedback_sent, &counters.feedback_dropped)
            };

            match sent {
                Ok(_) => delivered.increment(),
                Err(NetError::ControlWindowFull { .. }) => dropped.increment(),
                Err(error) => return Err(error.into()),
            }
        }

        self.outbox.flush()
    }

    fn release_all(&mut self, target: &mut dyn InputTarget) {
        let mut events = Vec::new();
        self.held.release_all(&mut events);

        self.inject_all(target, &events);
    }
}

#[cfg(test)]
mod tests {
    use std::{marker::PhantomData, net::SocketAddr, sync::Arc};

    use rusplay_input::{GamepadButton, GamepadEvent, RumbleCommand};
    use rusplay_net::{ControlChannel, ControlConfig, Transport, TransportOptions};
    use rusplay_protocol::push_input_record;

    use super::*;
    use crate::stats::HostStats;

    #[derive(Default)]
    struct Recorder {
        events: Vec<InputEvent>,
        failing: bool,
    }

    impl InputTarget for Recorder {
        fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
            if self.failing {
                return Err(InputError::SinkClosed);
            }
            self.events.push(event);
            Ok(())
        }

        fn service(&mut self, _feedback: &mut Vec<PadFeedback>) -> Result<(), InputError> {
            Ok(())
        }
    }

    struct Rig {
        shared: Arc<Shared>,
        outbox: Arc<ControlOutbox>,
    }

    impl Rig {
        fn new() -> Self {
            let network = rusplay_config::NetworkConfig::default();
            let transport = Transport::bind(
                SocketAddr::from(([127, 0, 0, 1], 0)),
                TransportOptions::from(&network),
            )
            .unwrap();
            let address = transport.local_addr().unwrap();

            let shared = Arc::new(Shared::new(address));
            let outbox = Arc::new(ControlOutbox::new(
                ControlChannel::new(ControlConfig::from(&network)),
                Arc::new(transport),
                Arc::clone(&shared.client),
            ));

            Self { shared, outbox }
        }

        fn stage(&self) -> InputStage<'_> {
            InputStage::new(&self.shared, &self.outbox)
        }

        fn stats(&self) -> HostStats {
            self.shared.counters.snapshot()
        }
    }

    fn key(keycode: u32, pressed: bool) -> InputEvent {
        InputEvent::Key { keycode, pressed }
    }

    fn section(records: &[InputRecord]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoded = Vec::new();
        for record in records {
            encoded.clear();
            record.encode_into(&mut encoded);
            push_input_record(&mut out, &encoded).unwrap();
        }
        out
    }

    fn datagram(sequence: u32, current: &[InputRecord], previous: &[InputRecord]) -> Vec<u8> {
        let current = section(current);
        let previous = section(previous);
        let mut out = Vec::new();
        InputPacket {
            sequence,
            current: &current,
            previous: &previous,
        }
        .write(&mut out)
        .unwrap();
        out
    }

    fn event(event: InputEvent) -> InputRecord {
        InputRecord::Event(event)
    }

    fn snapshot_of(events: &[InputEvent]) -> Vec<InputRecord> {
        let mut held = HeldInput::new();
        for event in events {
            held.observe(event);
        }
        let mut records = Vec::new();
        held.snapshot(&mut records);
        records.into_iter().map(InputRecord::State).collect()
    }

    struct ThreadBound {
        events: Arc<Mutex<Vec<InputEvent>>>,
        rumble: PadFeedback,
        _not_send: PhantomData<*const ()>,
    }

    impl InputTarget for ThreadBound {
        fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
            self.events.lock().push(event);
            Ok(())
        }

        fn service(&mut self, feedback: &mut Vec<PadFeedback>) -> Result<(), InputError> {
            feedback.push(self.rumble);
            Ok(())
        }
    }

    #[test]
    fn a_target_that_cannot_leave_its_thread_is_driven_through_the_injector_thread() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let rumble = PadFeedback::Rumble(RumbleCommand {
            slot: 1,
            strong: 7,
            weak: 3,
            delay_ms: 0,
            duration_ms: 100,
        });
        let recorded = Arc::clone(&events);
        let feed = InputFeed::spawn(move || {
            Ok(ThreadBound {
                events: recorded,
                rumble,
                _not_send: PhantomData,
            })
        })
        .unwrap();

        let mut commands = Vec::new();
        let outcome = {
            let mut target = feed.target();
            target.inject(key(30, true)).unwrap();
            target.service(&mut commands)
        };

        assert!(outcome.is_ok());
        assert_eq!(*events.lock(), vec![key(30, true)]);
        assert_eq!(commands, vec![rumble]);
    }

    struct StallsOnce {
        stalled: bool,
    }

    impl InputTarget for StallsOnce {
        fn inject(&mut self, _event: InputEvent) -> Result<(), InputError> {
            if !self.stalled {
                self.stalled = true;
                thread::sleep(INJECT_TIMEOUT + Duration::from_millis(100));
            }
            Ok(())
        }

        fn service(&mut self, _feedback: &mut Vec<PadFeedback>) -> Result<(), InputError> {
            Ok(())
        }
    }

    #[test]
    fn a_stalled_injector_times_out_and_its_late_reply_is_not_mistaken_for_the_next() {
        let feed = InputFeed::spawn(|| Ok(StallsOnce { stalled: false })).unwrap();
        let mut target = feed.target();

        let stalled = target.inject(key(30, true));
        let recovered = target.inject(key(30, false));

        assert!(matches!(stalled, Err(InputError::SinkClosed)));
        assert!(recovered.is_ok());
    }

    #[test]
    fn a_target_that_fails_to_build_reports_why() {
        let built = InputFeed::spawn(|| -> Result<Recorder, HostError> {
            Err(HostError::CaptureNotCompiled)
        });

        assert!(matches!(built, Err(HostError::CaptureNotCompiled)));
    }

    #[test]
    fn dropping_the_feed_stops_the_injector_thread_and_drops_the_target() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&events);
        let feed = InputFeed::spawn(move || {
            Ok(ThreadBound {
                events: recorded,
                rumble: PadFeedback::Rumble(RumbleCommand::stop(0)),
                _not_send: PhantomData,
            })
        })
        .unwrap();
        assert_eq!(Arc::strong_count(&events), 2);

        drop(feed);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&events) > 1 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(Arc::strong_count(&events), 1);
    }

    #[test]
    fn events_reach_the_target_in_order() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(
            &mut target,
            &datagram(0, &[event(key(30, true)), event(key(31, true))], &[]),
        );
        stage.apply_datagram(&mut target, &datagram(1, &[event(key(30, false))], &[]));

        assert_eq!(
            target.events,
            vec![key(30, true), key(31, true), key(30, false)]
        );
        assert_eq!(rig.stats().input_events, 3);
        assert_eq!(rig.stats().input_packets, 2);
    }

    #[derive(Default)]
    struct Batches {
        sizes: Vec<usize>,
        rejected: Vec<InputEvent>,
    }

    impl InputTarget for Batches {
        fn inject(&mut self, _event: InputEvent) -> Result<(), InputError> {
            Ok(())
        }

        fn inject_batch(
            &mut self,
            events: &[InputEvent],
            results: &mut Vec<Result<(), InputError>>,
        ) {
            self.sizes.push(events.len());
            results.clear();
            results.extend(events.iter().map(|event| {
                if self.rejected.contains(event) {
                    Err(InputError::SinkClosed)
                } else {
                    Ok(())
                }
            }));
        }

        fn service(&mut self, _feedback: &mut Vec<PadFeedback>) -> Result<(), InputError> {
            Ok(())
        }
    }

    #[test]
    fn the_events_of_a_datagram_reach_the_target_as_one_batch() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Batches::default();

        stage.apply_datagram(
            &mut target,
            &datagram(
                0,
                &[
                    event(key(1, true)),
                    event(key(2, true)),
                    event(key(3, true)),
                ],
                &[],
            ),
        );

        assert_eq!(target.sizes, vec![3]);
        assert_eq!(rig.stats().input_events, 3);
    }

    #[test]
    fn a_state_record_sees_the_outcome_of_the_events_before_it() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Batches::default();
        let mut records = vec![event(key(30, true))];
        records.extend(snapshot_of(&[]));

        stage.apply_datagram(&mut target, &datagram(0, &records, &[]));

        assert_eq!(target.sizes, vec![1, 1]);
        assert_eq!(rig.stats().input_events_repaired, 1);
    }

    #[test]
    fn each_event_of_a_batch_keeps_its_own_outcome() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Batches {
            rejected: vec![key(2, true)],
            ..Batches::default()
        };

        stage.apply_datagram(
            &mut target,
            &datagram(0, &[event(key(1, true)), event(key(2, true))], &[]),
        );
        stage.release_all(&mut target);

        assert_eq!(rig.stats().input_events, 2);
        assert_eq!(rig.stats().input_failures, 1);
        assert_eq!(target.sizes, vec![2, 1]);
    }

    struct RejectsOdd;

    impl InputTarget for RejectsOdd {
        fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
            match event {
                InputEvent::Key { keycode, .. } if keycode % 2 == 1 => Err(InputError::SinkClosed),
                _ => Ok(()),
            }
        }

        fn service(&mut self, _feedback: &mut Vec<PadFeedback>) -> Result<(), InputError> {
            Ok(())
        }
    }

    #[test]
    fn a_batch_through_the_injector_thread_returns_a_result_per_event() {
        let feed = InputFeed::spawn(|| Ok(RejectsOdd)).unwrap();
        let mut target = feed.target();
        let mut results = Vec::new();

        target.inject_batch(&[key(2, true), key(3, true), key(4, true)], &mut results);

        assert_eq!(
            results.iter().map(Result::is_ok).collect::<Vec<_>>(),
            vec![true, false, true]
        );
    }

    #[test]
    fn a_lost_datagram_is_recovered_from_the_next_ones_previous_section() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(&mut target, &datagram(0, &[event(key(1, true))], &[]));
        stage.apply_datagram(
            &mut target,
            &datagram(2, &[event(key(3, true))], &[event(key(2, true))]),
        );

        assert_eq!(
            target.events,
            vec![key(1, true), key(2, true), key(3, true)]
        );
    }

    #[test]
    fn duplicates_and_reordered_datagrams_are_dropped() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(&mut target, &datagram(5, &[event(key(1, true))], &[]));
        stage.apply_datagram(&mut target, &datagram(5, &[event(key(1, true))], &[]));
        stage.apply_datagram(&mut target, &datagram(4, &[event(key(2, true))], &[]));

        assert_eq!(target.events, vec![key(1, true)]);
        assert_eq!(rig.stats().input_packets_stale, 2);
    }

    #[test]
    fn a_new_client_address_restarts_the_datagram_sequence() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(&mut target, &datagram(50, &[event(key(1, true))], &[]));
        rig.shared
            .client
            .switch(SocketAddr::from(([127, 0, 0, 1], 9)));
        stage.follow_client(&mut target);
        target.events.clear();
        stage.apply_datagram(&mut target, &datagram(0, &[event(key(2, true))], &[]));

        assert_eq!(target.events, vec![key(2, true)]);
        assert_eq!(rig.stats().input_packets_stale, 0);
    }

    #[test]
    fn a_takeover_lifts_what_the_old_client_held() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();
        let pad = InputEvent::Gamepad {
            slot: 1,
            event: GamepadEvent::Connected,
        };

        stage.apply_datagram(
            &mut target,
            &datagram(0, &[event(key(30, true)), event(pad)], &[]),
        );
        target.events.clear();
        rig.shared
            .client
            .switch(SocketAddr::from(([127, 0, 0, 1], 9)));
        stage.follow_client(&mut target);
        let lifted = target.events.len();
        stage.apply_datagram(&mut target, &datagram(0, &snapshot_of(&[]), &[]));

        assert_eq!(lifted, 2);
        assert!(target.events.contains(&key(30, false)));
        assert_eq!(target.events.len(), 2);
        assert!(stage.held.is_idle());
    }

    #[test]
    fn datagrams_forwarded_for_the_old_client_are_dropped_after_a_takeover() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();
        let old_epoch = rig.shared.client.epoch();
        rig.shared
            .client
            .switch(SocketAddr::from(([127, 0, 0, 1], 9)));
        let new_epoch = rig.shared.client.epoch();

        stage.admit(
            &mut target,
            &ForwardedDatagram {
                epoch: old_epoch,
                bytes: datagram(900, &[event(key(1, true))], &[]),
            },
        );
        stage.admit(
            &mut target,
            &ForwardedDatagram {
                epoch: new_epoch,
                bytes: datagram(0, &[event(key(2, true))], &[]),
            },
        );

        assert_eq!(target.events, vec![key(2, true)]);
        assert_eq!(rig.stats().input_packets_stale, 1);
    }

    #[test]
    fn a_snapshot_releases_a_key_whose_release_was_lost_twice() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(&mut target, &datagram(0, &[event(key(30, true))], &[]));
        stage.apply_datagram(&mut target, &datagram(3, &snapshot_of(&[]), &[]));

        assert_eq!(target.events, vec![key(30, true), key(30, false)]);
        assert_eq!(rig.stats().input_events_repaired, 1);
        assert_eq!(rig.stats().input_state_records, 4);
    }

    #[test]
    fn a_snapshot_presses_a_key_whose_press_was_lost_and_builds_pads() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();
        let pad = |event| InputEvent::Gamepad { slot: 1, event };
        let records = snapshot_of(&[
            key(57, true),
            pad(GamepadEvent::Connected),
            pad(GamepadEvent::Button {
                button: GamepadButton::SOUTH,
                pressed: true,
            }),
        ]);

        stage.apply_datagram(&mut target, &datagram(0, &records, &[]));

        assert_eq!(
            target.events,
            vec![
                key(57, true),
                pad(GamepadEvent::Connected),
                pad(GamepadEvent::Button {
                    button: GamepadButton::SOUTH,
                    pressed: true,
                }),
            ]
        );
    }

    #[test]
    fn an_agreeing_snapshot_injects_nothing() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(&mut target, &datagram(0, &[event(key(30, true))], &[]));
        stage.apply_datagram(
            &mut target,
            &datagram(1, &snapshot_of(&[key(30, true)]), &[]),
        );

        assert_eq!(target.events, vec![key(30, true)]);
        assert_eq!(rig.stats().input_events_repaired, 0);
    }

    #[test]
    fn a_failed_release_is_retried_by_the_next_snapshot() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(&mut target, &datagram(0, &[event(key(30, true))], &[]));
        target.failing = true;
        stage.apply_datagram(&mut target, &datagram(1, &[event(key(30, false))], &[]));
        target.failing = false;
        stage.apply_datagram(&mut target, &datagram(2, &snapshot_of(&[]), &[]));

        assert_eq!(target.events, vec![key(30, true), key(30, false)]);
        assert_eq!(rig.stats().input_failures, 1);
    }

    #[test]
    fn malformed_records_are_counted_and_the_rest_still_applies() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();
        let mut current = Vec::new();
        push_input_record(&mut current, &[200]).unwrap();
        let mut good = Vec::new();
        event(key(9, true)).encode_into(&mut good);
        push_input_record(&mut current, &good).unwrap();
        let mut bytes = Vec::new();
        InputPacket {
            sequence: 0,
            current: &current,
            previous: &[],
        }
        .write(&mut bytes)
        .unwrap();

        stage.apply_datagram(&mut target, &bytes);
        stage.apply_datagram(&mut target, &bytes[..8]);

        assert_eq!(target.events, vec![key(9, true)]);
        assert_eq!(rig.stats().malformed_datagrams, 2);
    }

    #[test]
    fn ending_the_session_lifts_what_was_still_held() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(
            &mut target,
            &datagram(
                0,
                &[
                    event(key(30, true)),
                    event(InputEvent::PointerButton {
                        button: 0x110,
                        pressed: true,
                    }),
                    event(InputEvent::Gamepad {
                        slot: 2,
                        event: GamepadEvent::Connected,
                    }),
                ],
                &[],
            ),
        );
        target.events.clear();
        stage.release_all(&mut target);

        assert_eq!(
            target.events,
            vec![
                key(30, false),
                InputEvent::PointerButton {
                    button: 0x110,
                    pressed: false,
                },
                InputEvent::Gamepad {
                    slot: 2,
                    event: GamepadEvent::Disconnected,
                },
            ]
        );
    }
}
