use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_input::{GamepadButton, GamepadEvent, InputError, InputEvent, RumbleCommand};
use rusplay_protocol::{ControlMessage, InputPacket, push_input_record};

use super::{
    AtomicU32, Fixture, TestClient, WAIT, fake_factory, feed, host_config, pin, wait_for_end,
};
use crate::{InputFeed, InputTarget};

#[derive(Clone, Default)]
struct SharedTarget {
    events: Arc<Mutex<Vec<InputEvent>>>,
    rumble: Arc<Mutex<Vec<RumbleCommand>>>,
    failing: Arc<Mutex<bool>>,
}

impl InputTarget for SharedTarget {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        if *self.failing.lock() {
            return Err(InputError::SinkClosed);
        }
        self.events.lock().push(event);
        Ok(())
    }

    fn service(&mut self, rumble: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
        rumble.append(&mut self.rumble.lock());
        Ok(())
    }
}

impl SharedTarget {
    fn events(&self) -> Vec<InputEvent> {
        self.events.lock().clone()
    }

    fn wait_for(&self, count: usize) -> bool {
        let deadline = Instant::now() + WAIT;
        while self.events.lock().len() < count {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
        true
    }
}

fn key(keycode: u32, pressed: bool) -> InputEvent {
    InputEvent::Key { keycode, pressed }
}

fn input_datagram(sequence: u32, current: &[InputEvent], previous: &[InputEvent]) -> Vec<u8> {
    let section = |events: &[InputEvent]| {
        let mut out = Vec::new();
        let mut encoded = Vec::new();
        for event in events {
            encoded.clear();
            event.encode_into(&mut encoded);
            push_input_record(&mut out, &encoded).unwrap();
        }
        out
    };
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

fn fixture(target: &SharedTarget) -> Fixture {
    let target = target.clone();

    Fixture::with_session(host_config(), move |session| {
        session.without_audio().with_input(InputFeed::new(target))
    })
}

#[test]
fn input_datagrams_after_the_hello_reach_the_target_in_order() {
    let target = SharedTarget::default();
    let fixture = fixture(&target);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));
    client.send_datagram(&input_datagram(0, &[key(30, true), key(31, true)], &[]));
    client.send_datagram(&input_datagram(1, &[key(30, false)], &[]));

    assert!(target.wait_for(3));
    assert_eq!(
        target.events(),
        vec![key(30, true), key(31, true), key(30, false)]
    );
    assert_eq!(stream.stats().input_packets, 2);
    stream.stop().unwrap();
}

#[test]
fn input_before_the_hello_is_ignored() {
    let target = SharedTarget::default();
    let fixture = fixture(&target);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    client.send_datagram(&input_datagram(0, &[key(30, true)], &[]));
    client.pump(Duration::from_millis(200), |_| false);
    let early = target.events();
    client.hello();
    assert!(client.pump_until(TestClient::has_hello));
    client.send_datagram(&input_datagram(1, &[key(31, true)], &[]));

    assert!(target.wait_for(1));
    assert_eq!(early, Vec::new());
    assert_eq!(target.events(), vec![key(31, true)]);
    stream.stop().unwrap();
}

#[test]
fn rumble_from_the_target_reaches_the_client_on_the_control_channel() {
    let target = SharedTarget::default();
    let fixture = fixture(&target);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));
    let command = RumbleCommand {
        slot: 2,
        strong: 0xC000,
        weak: 0x2000,
        delay_ms: 5,
        duration_ms: 300,
    };
    target
        .rumble
        .lock()
        .extend([command, RumbleCommand::stop(2)]);

    let both = client.pump_until(|client| {
        client.has(ControlMessage::Rumble {
            slot: 2,
            strong: 0xC000,
            weak: 0x2000,
            delay_ms: 5,
            duration_ms: 300,
        }) && client.has(ControlMessage::Rumble {
            slot: 2,
            strong: 0,
            weak: 0,
            delay_ms: 0,
            duration_ms: 0,
        })
    });

    assert!(both);
    assert_eq!(stream.stats().rumble_sent, 2);
    stream.stop().unwrap();
}

#[test]
fn ending_the_session_releases_whatever_the_client_still_held() {
    let target = SharedTarget::default();
    let fixture = fixture(&target);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));
    let pad = |event| InputEvent::Gamepad { slot: 0, event };

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));
    client.send_datagram(&input_datagram(
        0,
        &[
            key(30, true),
            pad(GamepadEvent::Connected),
            pad(GamepadEvent::Button {
                button: GamepadButton::SOUTH,
                pressed: true,
            }),
        ],
        &[],
    ));
    assert!(target.wait_for(3));
    client.send(ControlMessage::Goodbye);

    wait_for_end(stream).unwrap();

    assert_eq!(
        target.events()[3..],
        [key(30, false), pad(GamepadEvent::Disconnected)]
    );
}

#[test]
fn a_target_that_rejects_events_is_counted_and_the_stream_goes_on() {
    let target = SharedTarget::default();
    *target.failing.lock() = true;
    let fixture = fixture(&target);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(5), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));
    client.send_datagram(&input_datagram(0, &[key(30, true)], &[]));

    let flowing = client.pump_until(|client| client.frames.len() >= 4);
    let deadline = Instant::now() + WAIT;
    while stream.stats().input_failures == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }

    assert!(flowing);
    assert_eq!(stream.stats().input_failures, 1);
    assert!(!stream.is_finished());
    stream.stop().unwrap();
}

#[test]
fn a_host_without_an_input_feed_ignores_input_datagrams() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));
    client.send_datagram(&input_datagram(0, &[key(30, true)], &[]));
    client.pump(Duration::from_millis(150), |_| false);

    assert_eq!(stream.stats().input_packets, 0);
    assert!(!stream.is_finished());
    stream.stop().unwrap();
}
