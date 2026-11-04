use std::{
    sync::{Arc, atomic::AtomicU32},
    thread,
    time::{Duration, Instant},
};

use rusplay_clipboard::{ClipboardBackend, ClipboardState, MemoryClipboard};
use rusplay_config::{ClipboardDirection, Config};
use rusplay_protocol::{ClipboardChunk, ClipboardReassembler, ControlMessage};

use super::{Fixture, TestClient, WAIT, fake_factory, feed, host_config, pin};
use crate::{ClipboardOpener, HostStream};

const LIMIT: u32 = 2048;

fn config_limited(direction: ClipboardDirection, limit: u32) -> Config {
    let mut config = host_config();
    config.input.clipboard = direction;
    config.input.clipboard_max_bytes = limit;

    config
}

fn config(direction: ClipboardDirection) -> Config {
    config_limited(direction, LIMIT)
}

fn opener(clipboard: &MemoryClipboard) -> ClipboardOpener {
    let clipboard = clipboard.clone();

    Arc::new(move |_| Ok(Box::new(clipboard.clone()) as Box<dyn ClipboardBackend>))
}

fn streaming(
    direction: ClipboardDirection,
    clipboard: &MemoryClipboard,
) -> (Fixture, TestClient, HostStream) {
    streaming_with(config(direction), clipboard)
}

fn streaming_with(
    config: Config,
    clipboard: &MemoryClipboard,
) -> (Fixture, TestClient, HostStream) {
    let opener = opener(clipboard);
    let fixture = Fixture::with_session(config, move |session| {
        session.without_audio().with_clipboard(opener)
    });
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));

    (fixture, client, stream)
}

fn send_text(client: &mut TestClient, transfer: u32, text: &str) {
    for chunk in ClipboardChunk::split(transfer, text).unwrap() {
        client.send(ControlMessage::Clipboard(chunk));
    }
}

fn wait_for(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while !condition() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }

    condition()
}

fn received_text(client: &TestClient) -> Option<String> {
    let mut reassembler = ClipboardReassembler::new(usize::MAX);

    client
        .messages
        .iter()
        .filter_map(|message| match message {
            ControlMessage::Clipboard(chunk) => Some(*chunk),
            _ => None,
        })
        .find_map(|chunk| reassembler.push(&chunk).unwrap())
}

#[test]
fn text_from_the_client_lands_in_the_host_clipboard() {
    let clipboard = MemoryClipboard::default();
    let (_fixture, mut client, stream) = streaming(ClipboardDirection::Both, &clipboard);
    let text = "héllo ".repeat(100);

    send_text(&mut client, 0, &text);

    assert!(wait_for(
        || clipboard.contents().as_deref() == Some(text.as_str())
    ));
    assert!(stream.clipboard().is_active());
    stream.stop().unwrap();
}

#[test]
fn text_copied_on_the_host_reaches_the_client_in_chunks_and_is_not_sent_twice() {
    let clipboard = MemoryClipboard::default();
    let (_fixture, mut client, stream) = streaming(ClipboardDirection::HostToClient, &clipboard);
    let text = "wörds ".repeat(200);

    clipboard.copy(text.clone());
    let arrived =
        client.pump_until(|client| received_text(client).as_deref() == Some(text.as_str()));
    let chunks = client
        .messages
        .iter()
        .filter(|message| matches!(message, ControlMessage::Clipboard(_)))
        .count();

    assert!(arrived);
    assert_eq!(chunks, text.len().div_ceil(ClipboardChunk::MAX_DATA));
    stream.stop().unwrap();
}

#[test]
fn nothing_is_applied_when_the_direction_does_not_let_the_client_send() {
    let clipboard = MemoryClipboard::default();
    let (_fixture, mut client, stream) = streaming(ClipboardDirection::HostToClient, &clipboard);

    send_text(&mut client, 0, "pushed at the host");
    thread::sleep(Duration::from_millis(300));

    assert_eq!(clipboard.contents(), None);
    assert_eq!(clipboard.writes(), 0);
    stream.stop().unwrap();
}

#[test]
fn nothing_is_read_from_the_host_when_the_direction_does_not_let_it_send() {
    let clipboard = MemoryClipboard::default();
    let (_fixture, mut client, stream) = streaming(ClipboardDirection::ClientToHost, &clipboard);

    clipboard.copy("private to the host");
    client.pump(Duration::from_millis(300), |_| false);

    assert!(received_text(&client).is_none());
    assert_eq!(clipboard.polls(), 0);
    stream.stop().unwrap();
}

#[test]
fn invalid_utf8_is_refused_and_counted_malformed() {
    let clipboard = MemoryClipboard::default();
    let (_fixture, mut client, stream) = streaming(ClipboardDirection::Both, &clipboard);

    let chunk = ClipboardChunk::new(0, 0, 1, &[0xFF, 0xFE, b'a']).unwrap();
    client.send(ControlMessage::Clipboard(chunk));

    assert!(wait_for(|| stream.stats().malformed_datagrams >= 1));
    assert_eq!(clipboard.writes(), 0);
    stream.stop().unwrap();
}

#[test]
fn a_transfer_larger_than_the_limit_is_refused_without_buffering_it() {
    let clipboard = MemoryClipboard::default();
    let (_fixture, mut client, stream) = streaming(ClipboardDirection::Both, &clipboard);

    send_text(
        &mut client,
        0,
        &"a".repeat(usize::try_from(LIMIT).unwrap() + 1),
    );

    assert!(wait_for(|| stream.stats().malformed_datagrams >= 1));
    assert_eq!(clipboard.writes(), 0);
    stream.stop().unwrap();
}

#[test]
fn text_received_from_the_client_is_not_sent_back() {
    let clipboard = MemoryClipboard::default();
    let (_fixture, mut client, stream) = streaming(ClipboardDirection::Both, &clipboard);

    send_text(&mut client, 0, "one way only");
    assert!(wait_for(|| clipboard.writes() == 1));
    client.pump(Duration::from_millis(300), |_| false);

    assert!(received_text(&client).is_none());
    stream.stop().unwrap();
}

#[test]
fn an_unavailable_backend_is_reported_and_the_session_streams_on() {
    let opener: ClipboardOpener =
        Arc::new(|_| Err(rusplay_clipboard::ClipboardError::DataControlMissing));
    let fixture = Fixture::with_session(config(ClipboardDirection::Both), move |session| {
        session.without_audio().with_clipboard(opener)
    });
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(40), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let streams = client.pump_until(|client| client.frames.len() >= 2);

    assert!(streams);
    assert!(matches!(stream.clipboard(), ClipboardState::Unavailable(_)));
    stream.stop().unwrap();
}

#[test]
fn a_text_needing_more_chunks_than_the_control_window_still_arrives_whole() {
    let clipboard = MemoryClipboard::default();
    let config = config_limited(ClipboardDirection::HostToClient, 65_536);
    let (_fixture, mut client, stream) = streaming_with(config, &clipboard);
    let text = "0123456789abcdef".repeat(3_500);

    clipboard.copy(text.clone());
    let arrived =
        client.pump_until(|client| received_text(client).as_deref() == Some(text.as_str()));

    assert!(arrived);
    assert!(text.len().div_ceil(ClipboardChunk::MAX_DATA) > 64);
    stream.stop().unwrap();
}
