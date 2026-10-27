use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_audio::{
    AudioEncoder, AudioError, AudioOutput, AudioOutputStats, CODEC_THREAD_STACK_BYTES,
    OpusAudioEncoder,
};
use rusplay_config::{AudioConfig, Config, MicrophoneConfig};
use rusplay_net::{FecScheme, Packetizer};
use rusplay_protocol::StreamKind;

use super::{AtomicU32, Fixture, TestClient, WAIT, fake_factory, feed, host_config, pin};
use crate::{HostError, HostSession, MicrophoneOutputFactory};

const AMPLITUDE: f32 = 0.5;
const TONE_STEP: f32 = 0.05;
const PACKETS: u32 = 12;
const PACKET_BUFFER: usize = 4000;

#[derive(Clone, Default)]
struct Capture {
    frames: Arc<Mutex<Vec<Vec<f32>>>>,
    broken: Arc<AtomicBool>,
}

impl AudioOutput for Capture {
    fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError> {
        if self.broken.load(Ordering::Relaxed) {
            return Err(AudioError::FrameLengthMismatch {
                expected: 0,
                found: pcm.len(),
            });
        }
        self.frames.lock().push(pcm.to_vec());
        Ok(0)
    }

    fn stats(&self) -> AudioOutputStats {
        AudioOutputStats::default()
    }
}

impl Capture {
    fn wait_for_frames(&self, count: usize) -> bool {
        let deadline = Instant::now() + WAIT;
        while self.frames.lock().len() < count {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
        true
    }

    fn peak(&self) -> f32 {
        self.frames
            .lock()
            .iter()
            .flatten()
            .fold(0.0, |peak: f32, sample| peak.max(sample.abs()))
    }
}

fn factory(capture: &Capture, opened: &Arc<AtomicUsize>) -> MicrophoneOutputFactory {
    let capture = capture.clone();
    let opened = Arc::clone(opened);
    Arc::new(move |config| {
        assert_eq!(config.channels.channel_count(), 1);
        opened.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(capture.clone()) as Box<dyn AudioOutput>)
    })
}

fn microphone_config(enabled: bool) -> Config {
    let mut config = host_config();
    config.audio.microphone = MicrophoneConfig {
        enabled,
        ..MicrophoneConfig::default()
    };
    config
}

fn fixture(config: Config, capture: &Capture, opened: &Arc<AtomicUsize>) -> Fixture {
    let factory = factory(capture, opened);
    Fixture::with_session(config, move |session: HostSession| {
        session.without_audio().with_microphone_output(factory)
    })
}

fn tone_packets(config: &AudioConfig, count: u32) -> Vec<Vec<u8>> {
    let stream = config.microphone_stream();
    thread::Builder::new()
        .stack_size(CODEC_THREAD_STACK_BYTES)
        .spawn(move || {
            let mut encoder = OpusAudioEncoder::new(&stream).unwrap();
            let samples = encoder.frame_samples();
            (0..count as usize)
                .map(|number| {
                    let tone: Vec<f32> = (0..samples)
                        .map(|index| {
                            let position = number * samples + index;
                            (f32::from(u16::try_from(position % 60_000).unwrap()) * TONE_STEP).sin()
                                * AMPLITUDE
                        })
                        .collect();
                    let mut packet = vec![0; PACKET_BUFFER];
                    let written = encoder.encode(&tone, &mut packet).unwrap();
                    packet.truncate(written);
                    packet
                })
                .collect()
        })
        .unwrap()
        .join()
        .unwrap()
}

fn send_packets(client: &TestClient, packets: &[Vec<u8>], scheme: FecScheme) {
    let mut packetizer = Packetizer::new(
        StreamKind::Microphone,
        client.transport.max_datagram_len(&client.host),
        scheme,
    )
    .unwrap();

    for (number, packet) in (0..).zip(packets) {
        for datagram in packetizer.packetize(number, false, packet).unwrap() {
            client.send_datagram(&datagram);
        }
    }
}

#[test]
fn microphone_packets_after_the_hello_play_into_the_output() {
    let capture = Capture::default();
    let opened = Arc::new(AtomicUsize::new(0));
    let config = microphone_config(true);
    let packets = tone_packets(&config.audio, PACKETS);
    let fixture = fixture(config, &capture, &opened);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    client.hello();
    assert!(client.pump_until(TestClient::has_hello));
    send_packets(&client, &packets, FecScheme::None);

    let played = capture.wait_for_frames(PACKETS as usize);
    let stats = stream.stats();

    assert!(played);
    assert_eq!(opened.load(Ordering::Relaxed), 1);
    assert!(capture.peak() > AMPLITUDE / 2.0);
    assert!(
        capture
            .frames
            .lock()
            .iter()
            .all(|frame| frame.len() == config_frame_samples())
    );
    assert!(stats.microphone_packets_received >= u64::from(PACKETS));
    assert_eq!(stats.microphone_packets_lost, 0);
    stream.stop().unwrap();
}

fn config_frame_samples() -> usize {
    AudioConfig::default().microphone_stream().frame_samples()
}

#[test]
fn microphone_packets_with_fec_still_play() {
    let capture = Capture::default();
    let opened = Arc::new(AtomicUsize::new(0));
    let config = microphone_config(true);
    let packets = tone_packets(&config.audio, PACKETS);
    let fixture = fixture(config, &capture, &opened);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    client.hello();
    assert!(client.pump_until(TestClient::has_hello));
    send_packets(
        &client,
        &packets,
        FecScheme::ReedSolomon {
            data_shards: 4,
            parity_shards: 1,
        },
    );

    assert!(capture.wait_for_frames(PACKETS as usize));
    assert_eq!(stream.stats().microphone_packets_lost, 0);
    stream.stop().unwrap();
}

#[test]
fn microphone_packets_before_the_hello_are_ignored() {
    let capture = Capture::default();
    let opened = Arc::new(AtomicUsize::new(0));
    let config = microphone_config(true);
    let packets = tone_packets(&config.audio, 4);
    let fixture = fixture(config, &capture, &opened);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    send_packets(&client, &packets, FecScheme::None);
    thread::sleep(Duration::from_millis(150));

    assert!(capture.frames.lock().is_empty());
    assert_eq!(stream.stats().microphone_packets_received, 0);
    stream.stop().unwrap();
}

#[test]
fn a_disabled_microphone_opens_nothing_and_drops_the_packets() {
    let capture = Capture::default();
    let opened = Arc::new(AtomicUsize::new(0));
    let config = microphone_config(false);
    let packets = tone_packets(&config.audio, 4);
    let fixture = fixture(config, &capture, &opened);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    client.hello();
    assert!(client.pump_until(TestClient::has_hello));
    send_packets(&client, &packets, FecScheme::None);
    thread::sleep(Duration::from_millis(150));

    assert_eq!(opened.load(Ordering::Relaxed), 0);
    assert!(capture.frames.lock().is_empty());
    stream.stop().unwrap();
}

#[test]
fn an_enabled_microphone_without_an_output_refuses_the_session() {
    let fixture = Fixture::with_session(microphone_config(true), |session| {
        session.without_audio().without_microphone()
    });
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (_client, _) = TestClient::pair(fixture.address);
    let result = serving.join().unwrap();

    assert!(matches!(result, Err(HostError::MicrophoneNotCompiled)));
}

#[test]
fn a_failing_microphone_output_ends_the_session() {
    let capture = Capture::default();
    capture.broken.store(true, Ordering::Relaxed);
    let opened = Arc::new(AtomicUsize::new(0));
    let config = microphone_config(true);
    let packets = tone_packets(&config.audio, 2);
    let fixture = fixture(config, &capture, &opened);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    let (_, stream) = serving.join().unwrap().unwrap();
    client.hello();
    assert!(client.pump_until(TestClient::has_hello));
    send_packets(&client, &packets, FecScheme::None);

    assert!(matches!(
        super::wait_for_end(stream),
        Err(HostError::Audio(AudioError::FrameLengthMismatch { .. }))
    ));
}
