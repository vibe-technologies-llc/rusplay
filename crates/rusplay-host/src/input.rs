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
use rusplay_input::{HeldInput, InputError, InputEvent, InputRecord, RumbleCommand};
use rusplay_net::NetError;
use rusplay_protocol::{ControlMessage, InputAdmission, InputPacket, InputRecords, InputSequence};

use crate::{HostError, Stage, outbox::ControlOutbox, shared::Shared};

const SERVICE_INTERVAL: Duration = Duration::from_millis(4);
const IDLE_SERVICE_INTERVAL: Duration = Duration::from_millis(50);
const INJECTOR_THREAD: &str = "rusplay-host-injector";

pub trait InputTarget {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError>;

    fn service(&mut self, rumble: &mut Vec<RumbleCommand>) -> Result<(), InputError>;
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
            Ok(Ok(())) => Ok(Self::new(RemoteTarget { requests, replies })),
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
    Inject(InputEvent),
    Service,
}

enum Reply {
    Injected(Result<(), InputError>),
    Serviced {
        rumble: Vec<RumbleCommand>,
        result: Result<(), InputError>,
    },
}

struct RemoteTarget {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
}

impl RemoteTarget {
    fn call(&self, request: Request) -> Result<Reply, InputError> {
        self.requests
            .send(request)
            .map_err(|_| InputError::SinkClosed)?;

        self.replies.recv().map_err(|_| InputError::SinkClosed)
    }
}

impl InputTarget for RemoteTarget {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        match self.call(Request::Inject(event))? {
            Reply::Injected(result) => result,
            Reply::Serviced { .. } => Err(InputError::SinkClosed),
        }
    }

    fn service(&mut self, rumble: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
        match self.call(Request::Service)? {
            Reply::Serviced {
                rumble: commands,
                result,
            } => {
                rumble.extend(commands);
                result
            }
            Reply::Injected(_) => Err(InputError::SinkClosed),
        }
    }
}

fn serve_requests(
    mut target: impl InputTarget,
    requests: &Receiver<Request>,
    replies: &Sender<Reply>,
) {
    let mut idle = Vec::new();

    loop {
        match requests.recv_timeout(IDLE_SERVICE_INTERVAL) {
            Ok(Request::Inject(event)) => {
                replies.send(Reply::Injected(target.inject(event))).ok();
            }
            Ok(Request::Service) => {
                let mut rumble = Vec::new();
                let result = target.service(&mut rumble);
                replies.send(Reply::Serviced { rumble, result }).ok();
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
    pub datagrams: Receiver<Vec<u8>>,
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
    held: HeldInput,
    repairs: Vec<InputEvent>,
    commands: Vec<RumbleCommand>,
}

impl<'a> InputStage<'a> {
    fn new(shared: &'a Shared, outbox: &'a ControlOutbox) -> Self {
        Self {
            shared,
            outbox,
            sequence: InputSequence::new(),
            held: HeldInput::new(),
            repairs: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn serve(
        &mut self,
        target: &mut dyn InputTarget,
        datagrams: &Receiver<Vec<u8>>,
    ) -> Result<(), HostError> {
        while !self.shared.stopped() {
            match datagrams.recv_timeout(SERVICE_INTERVAL) {
                Ok(datagram) => {
                    self.apply_datagram(target, &datagram);
                    while let Ok(next) = datagrams.try_recv() {
                        self.apply_datagram(target, &next);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }

            self.service(target)?;
        }

        Ok(())
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
        for bytes in records {
            let decoded = bytes.ok().and_then(|bytes| InputRecord::decode(bytes).ok());

            match decoded {
                Some(record) => self.apply(target, record),
                None => self.shared.counters.malformed_datagrams.increment(),
            }
        }
    }

    fn apply(&mut self, target: &mut dyn InputTarget, record: InputRecord) {
        match record {
            InputRecord::Event(event) => {
                self.inject(target, event);
            }
            InputRecord::State(state) => {
                self.shared.counters.input_state_records.increment();

                let mut events = std::mem::take(&mut self.repairs);
                events.clear();
                self.held.reconcile(&state, &mut events);
                for event in events.drain(..) {
                    if self.inject(target, event) {
                        self.shared.counters.input_events_repaired.increment();
                    }
                }
                self.repairs = events;
            }
        }
    }

    fn inject(&mut self, target: &mut dyn InputTarget, event: InputEvent) -> bool {
        let counters = &self.shared.counters;
        let injected = target.inject(event).is_ok();

        if injected {
            self.held.observe(&event);
            counters.input_events.increment();
        } else {
            counters.input_failures.increment();
        }

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

        for command in self.commands.drain(..) {
            let message = ControlMessage::Rumble {
                slot: command.slot,
                strong: command.strong,
                weak: command.weak,
                delay_ms: command.delay_ms,
                duration_ms: command.duration_ms,
            };
            let sent = self.outbox.channel().send(message, Instant::now());

            match sent {
                Ok(_) => counters.rumble_sent.increment(),
                Err(NetError::ControlWindowFull { .. }) => counters.rumble_dropped.increment(),
                Err(error) => return Err(error.into()),
            }
        }

        self.outbox.flush()
    }

    fn release_all(&mut self, target: &mut dyn InputTarget) {
        let mut events = Vec::new();
        self.held.release_all(&mut events);

        for event in events {
            if target.inject(event).is_ok() {
                self.held.observe(&event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{marker::PhantomData, net::SocketAddr, sync::Arc};

    use rusplay_input::{GamepadButton, GamepadEvent};
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

        fn service(&mut self, _rumble: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
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

            Self {
                shared: Arc::new(Shared::new()),
                outbox: Arc::new(ControlOutbox::new(
                    ControlChannel::new(ControlConfig::from(&network)),
                    Arc::new(transport),
                    address,
                )),
            }
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
        rumble: RumbleCommand,
        _not_send: PhantomData<*const ()>,
    }

    impl InputTarget for ThreadBound {
        fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
            self.events.lock().push(event);
            Ok(())
        }

        fn service(&mut self, rumble: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
            rumble.push(self.rumble);
            Ok(())
        }
    }

    #[test]
    fn a_target_that_cannot_leave_its_thread_is_driven_through_the_injector_thread() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let rumble = RumbleCommand {
            slot: 1,
            strong: 7,
            weak: 3,
            delay_ms: 0,
            duration_ms: 100,
        };
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
                rumble: RumbleCommand::stop(0),
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
    fn a_snapshot_releases_a_key_whose_release_was_lost_twice() {
        let rig = Rig::new();
        let mut stage = rig.stage();
        let mut target = Recorder::default();

        stage.apply_datagram(&mut target, &datagram(0, &[event(key(30, true))], &[]));
        stage.apply_datagram(&mut target, &datagram(3, &snapshot_of(&[]), &[]));

        assert_eq!(target.events, vec![key(30, true), key(30, false)]);
        assert_eq!(rig.stats().input_events_repaired, 1);
        assert_eq!(rig.stats().input_state_records, 3);
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
