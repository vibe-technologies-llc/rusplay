use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use rusplay_audio::{AudioDecoder, AudioError, CODEC_THREAD_STACK_BYTES, OpusAudioDecoder};
use rusplay_capture::{CursorBitmap, CursorUpdate, Publisher, latest};
use rusplay_config::AudioConfig;
use rusplay_net::ReassembledFrame;
use rusplay_protocol::ControlMessage;

use super::{
    AtomicU32, Fixture, TestClient, fake_factory, feed, feed_with_cursor, host_config, pin,
    wait_for_end,
};
use crate::{AudioSource, AudioSourceFactory, HostError};

const AMPLITUDE: f32 = 0.5;
const TONE_STEP: f32 = 0.05;

struct ToneSource {
    next: Instant,
    interval: Duration,
    phase: f32,
}

impl ToneSource {
    fn new(config: &AudioConfig) -> Self {
        let samples = config.opus.frame_duration.samples_at(config.sample_rate);
        let nanos = samples as u64 * 1_000_000_000 / u64::from(config.sample_rate.hz());

        Self {
            next: Instant::now(),
            interval: Duration::from_nanos(nanos),
            phase: 0.0,
        }
    }
}

impl AudioSource for ToneSource {
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, AudioError> {
        let wait = self.next.saturating_duration_since(Instant::now());
        if wait > timeout {
            thread::sleep(timeout);
            return Ok(false);
        }
        thread::sleep(wait);

        let channels = 2;
        for samples in frame.chunks_exact_mut(channels) {
            let value = (self.phase * TONE_STEP).sin() * AMPLITUDE;
            samples.fill(value);
            self.phase += 1.0;
        }
        self.next += self.interval;

        Ok(true)
    }
}

struct StarvedSource;

impl AudioSource for StarvedSource {
    fn read_frame(&mut self, _frame: &mut [f32], timeout: Duration) -> Result<bool, AudioError> {
        thread::sleep(timeout);
        Ok(false)
    }
}

struct BrokenSource;

impl AudioSource for BrokenSource {
    fn read_frame(&mut self, frame: &mut [f32], _timeout: Duration) -> Result<bool, AudioError> {
        Err(AudioError::FrameLengthMismatch {
            expected: 0,
            found: frame.len(),
        })
    }
}

fn audio_factory<S: AudioSource + 'static>(
    make: impl Fn(&AudioConfig) -> S + Send + Sync + 'static,
) -> AudioSourceFactory {
    Arc::new(move |config| Ok(Box::new(make(config)) as Box<dyn AudioSource>))
}

fn bitmap(marker: u8) -> CursorBitmap {
    CursorBitmap {
        width: 2,
        height: 2,
        hotspot_x: 1,
        hotspot_y: 1,
        rgba: vec![marker; 16],
    }
}

fn update(x: i32, bitmap: Option<CursorBitmap>) -> CursorUpdate {
    CursorUpdate {
        x,
        y: 2 * x,
        visible: true,
        bitmap,
    }
}

fn publish(publisher: &Publisher<CursorUpdate>, update: CursorUpdate) {
    publisher.merge(update, CursorUpdate::absorb);
}

fn final_peak(packets: &[ReassembledFrame]) -> f32 {
    let packets: Vec<Vec<u8>> = packets.iter().map(|packet| packet.data.clone()).collect();

    thread::Builder::new()
        .stack_size(CODEC_THREAD_STACK_BYTES)
        .spawn(move || {
            let mut decoder = OpusAudioDecoder::new(&AudioConfig::default()).unwrap();
            let mut pcm = vec![0.0; 480];
            for packet in &packets {
                decoder.decode(packet, &mut pcm).unwrap();
            }
            pcm.iter()
                .fold(0.0, |peak: f32, sample| peak.max(sample.abs()))
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn audio_waits_for_the_hello_then_counts_up_from_zero() {
    let fixture = Fixture::with_session(host_config(), |session| {
        session.with_audio_source(audio_factory(ToneSource::new))
    });
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    let early = client.pump(Duration::from_millis(300), |client| {
        !client.audio_packets.is_empty()
    });
    client.hello();
    let streamed = client.pump_until(|client| {
        client.audio_packets.len() >= 12 && stream.stats().audio_packets_sent >= 12
    });
    let stats = stream.stats();

    assert!(!early);
    assert!(streamed);
    assert!(
        client
            .audio_packets
            .iter()
            .zip(0..)
            .all(|(packet, number)| packet.frame == number)
    );
    assert!(final_peak(&client.audio_packets[..12]) > AMPLITUDE / 2.0);
    assert!(stats.audio_packets_sent >= 12);
    stream.stop().unwrap();
}

#[test]
fn video_keeps_flowing_while_the_audio_source_delivers_nothing() {
    let fixture = Fixture::with_session(host_config(), |session| {
        session.with_audio_source(audio_factory(|_| StarvedSource))
    });
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(5), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let flowing = client.pump_until(|client| client.frames.len() >= 4);

    assert!(flowing);
    assert!(matches!(client.audio_packets.as_slice(), []));
    assert_eq!(stream.stats().audio_packets_sent, 0);
    stream.stop().unwrap();
}

#[test]
fn a_failing_audio_source_ends_the_session() {
    let fixture = Fixture::with_session(host_config(), |session| {
        session.with_audio_source(audio_factory(|_| BrokenSource))
    });
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (_client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();

    assert!(matches!(
        wait_for_end(stream),
        Err(HostError::Audio(AudioError::FrameLengthMismatch { .. }))
    ));
}

#[test]
fn the_first_cursor_message_carries_the_image_and_later_ones_only_the_position() {
    let mut config = host_config();
    config.network.cursor_image_repeat_ms = 0;
    let fixture = Fixture::new(config);
    let (publisher, subscriber) = latest();
    publish(&publisher, update(1, Some(bitmap(9))));
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(
        Some(pin()),
        feed_with_cursor(1, subscriber),
        fake_factory(&bitrate),
    );

    let (mut client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    let early = client.pump(Duration::from_millis(200), |client| {
        !client.cursor_messages.is_empty()
    });
    client.hello();
    client.pump_until(|client| !client.cursor_messages.is_empty());
    publish(&publisher, update(5, None));
    let moved = client.pump_until(|client| client.cursor_messages.len() >= 2);
    publish(&publisher, update(8, None));
    client.pump_until(|client| {
        client.cursor_messages.len() >= 3 && stream.stats().cursor_messages_sent >= 3
    });
    let stats = stream.stats();

    let image = client.cursor_messages[0].image.as_ref().unwrap();
    assert!(!early);
    assert!(moved);
    assert_eq!((image.width, image.height), (2, 2));
    assert_eq!(image.rgba, vec![9; 16]);
    assert_eq!(client.cursor_messages[1].x, 5);
    assert_eq!(client.cursor_messages[1].image, None);
    assert_eq!(client.cursor_messages[2].x, 8);
    assert_eq!(client.cursor_messages[2].image, None);
    assert_eq!(stats.cursor_images_sent, 1);
    assert_eq!(stats.cursor_messages_sent, 3);
    stream.stop().unwrap();
}

#[test]
fn a_burst_of_cursor_updates_ends_on_the_newest_position() {
    let mut config = host_config();
    config.network.cursor_image_repeat_ms = 0;
    let fixture = Fixture::new(config);
    let (publisher, subscriber) = latest();
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(
        Some(pin()),
        feed_with_cursor(1, subscriber),
        fake_factory(&bitrate),
    );

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    for x in 1..=200 {
        publish(&publisher, update(x, (x == 1).then(|| bitmap(4))));
    }
    let arrived = client.pump_until(|client| {
        client
            .cursor_messages
            .last()
            .is_some_and(|message| message.x == 200)
    });

    assert!(arrived);
    assert!(client.cursor_messages.len() <= 200);
    assert!(client.cursor_messages[0].image.is_some());
    stream.stop().unwrap();
}

#[test]
fn the_cursor_image_is_repeated_on_the_configured_interval() {
    let mut config = host_config();
    config.network.cursor_image_repeat_ms = 60;
    let fixture = Fixture::new(config);
    let (publisher, subscriber) = latest();
    publish(&publisher, update(1, Some(bitmap(3))));
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(
        Some(pin()),
        feed_with_cursor(1, subscriber),
        fake_factory(&bitrate),
    );

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let repeated = client.pump_until(|client| {
        client
            .cursor_messages
            .iter()
            .filter(|message| message.image.is_some())
            .count()
            >= 3
    });

    assert!(repeated);
    assert!(
        client
            .cursor_messages
            .iter()
            .all(|message| message.x == 1
                && message.image.as_ref().is_none_or(|i| i.rgba == [3; 16]))
    );
    stream.stop().unwrap();
}

#[test]
fn a_returning_client_gets_the_image_the_previous_session_already_saw() {
    let mut config = host_config();
    config.network.cursor_image_repeat_ms = 0;
    let fixture = Fixture::new(config);
    let (publisher, subscriber) = latest();
    let feed = feed_with_cursor(1, subscriber);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), Arc::clone(&feed), fake_factory(&bitrate));

    let (mut client, record) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    publish(&publisher, update(1, Some(bitmap(6))));
    client.pump_until(|client| !client.cursor_messages.is_empty());
    publish(&publisher, update(2, None));
    client.pump_until(|client| client.cursor_messages.len() >= 2);
    stream.stop().unwrap();

    let serving = fixture.serve(None, feed, fake_factory(&bitrate));
    let mut returning = TestClient::reconnect(fixture.address, &record);
    returning.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    returning.pump_until(|client| !client.cursor_messages.is_empty());

    let first = &returning.cursor_messages[0];
    assert_eq!(first.x, 2);
    assert_eq!(first.image.as_ref().unwrap().rgba, vec![6; 16]);
    stream.stop().unwrap();
}

#[test]
fn a_feed_without_a_cursor_stream_starts_no_cursor_stage() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(2), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    client.pump_until(|client| client.frames.len() >= 2);

    assert!(matches!(client.cursor_messages.as_slice(), []));
    assert!(!client.has(ControlMessage::Goodbye));
    assert_eq!(stream.stats().cursor_messages_sent, 0);
    stream.stop().unwrap();
}
