use std::{
    sync::{Arc, atomic::AtomicU32},
    thread,
    time::{Duration, Instant},
};

use rusplay_config::Config;
use rusplay_net::{PeerStore, Pin};
use rusplay_protocol::ControlMessage;
use tempfile::TempDir;

use super::{POLL, TestClient, WAIT, fake_factory, feed, host_config};
use crate::{Admission, Client, HostEvent, HostService, HostSession};

fn service(config: Config) -> (TempDir, HostService) {
    let directory = TempDir::new().unwrap();
    let store = PeerStore::open(directory.path().join("paired-clients")).unwrap();
    let host = HostSession::bind(config, store).unwrap();
    let bitrate = Arc::new(AtomicU32::new(0));
    let frames = feed(10_000);
    let serve = move |host: &HostSession, client: &Client| {
        host.serve_with(client, &frames, fake_factory(&bitrate))
    };

    (directory, HostService::spawn(host, serve, None).unwrap())
}

fn wait_for(
    service: &HostService,
    seen: &mut Vec<HostEvent>,
    wanted: impl Fn(&HostEvent) -> bool,
) -> bool {
    let deadline = Instant::now() + WAIT;

    loop {
        seen.extend(service.poll_events());
        if seen.iter().any(&wanted) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL);
    }
}

fn wait_until(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;

    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL);
    }

    true
}

fn opened_pin(event: &HostEvent) -> Option<Pin> {
    match event {
        HostEvent::PairingOpened { pin, .. } => Some(pin.clone()),
        _ => None,
    }
}

#[test]
fn disconnecting_ends_the_stream_and_the_next_window_accepts_the_same_client_again() {
    let (_directory, service) = service(host_config());
    let mut seen = Vec::new();

    assert!(wait_for(&service, &mut seen, |event| opened_pin(event).is_some()));
    let first = seen.iter().find_map(opened_pin).unwrap();
    let (mut client, record) = TestClient::pair_with(service.address(), &first);
    client.hello();
    let streaming = wait_until(|| service.snapshot().is_some());
    let admitted = wait_for(&service, &mut seen, |event| {
        matches!(event, HostEvent::ClientAdmitted { .. })
    });
    let before = seen.len();
    service.disconnect();
    let left = wait_for(&service, &mut seen, |event| {
        matches!(event, HostEvent::ClientLeft { .. })
    });
    let reopened = wait_for(&service, &mut seen, |event| {
        opened_pin(event).is_some_and(|pin| pin != first)
    });
    let farewell = client.pump_until(|client| client.has(ControlMessage::Goodbye));
    let idle = wait_until(|| service.snapshot().is_none());

    let mut returning = TestClient::reconnect(service.address(), &record);
    returning.hello();
    let readmitted = wait_for(&service, &mut seen, |event| {
        matches!(
            event,
            HostEvent::ClientAdmitted {
                admission: Admission::Reconnected,
                ..
            }
        )
    });
    let running = !service.is_finished();

    assert!(streaming);
    assert!(admitted);
    assert!(left);
    assert!(reopened);
    assert!(farewell);
    assert!(idle);
    assert!(readmitted);
    assert!(running);
    assert!(
        !seen[before..]
            .iter()
            .any(|event| matches!(event, HostEvent::Stopped(_) | HostEvent::SessionFailed(_)))
    );
    service.stop();
}

#[test]
fn disconnecting_with_nobody_streaming_leaves_the_pairing_window_open() {
    let (_directory, service) = service(host_config());
    let mut seen = Vec::new();
    assert!(wait_for(&service, &mut seen, |event| opened_pin(event).is_some()));

    service.disconnect();
    thread::sleep(Duration::from_millis(300));
    seen.extend(service.poll_events());

    let windows = seen
        .iter()
        .filter(|event| opened_pin(event).is_some())
        .count();
    assert_eq!(windows, 1);
    assert!(!service.is_finished());
    service.stop();
}
