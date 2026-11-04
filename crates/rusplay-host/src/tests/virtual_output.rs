use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_capture::{CaptureError, DrmFourcc, Frame, FrameBuffer, OutputOffer, OutputRequest};
use rusplay_config::SourceKind;
use rusplay_core::{ColorSpace, FrameRate, Resolution, Timestamp};
use rusplay_protocol::{ClientDisplay, ControlMessage, DisplayVolume};

use super::{
    AtomicU32, CountingSource, Fixture, TestClient, fake_factory, host_config, pin, stream_started,
};
use crate::{CaptureFeed, FrameSource};

const SHORT_WAIT: Duration = Duration::from_millis(300);

struct OfferingSource {
    inner: CountingSource,
    offers: Arc<Mutex<Vec<OutputRequest>>>,
}

impl FrameSource for OfferingSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        self.inner.next_frame(timeout)
    }

    fn output_offer(&self) -> Option<OutputOffer> {
        let offers = Arc::clone(&self.offers);

        Some(OutputOffer::new(move |request| offers.lock().push(request)))
    }
}

fn offering_feed(frames: u32) -> (Arc<CaptureFeed>, Arc<Mutex<Vec<OutputRequest>>>) {
    let offers = Arc::new(Mutex::new(Vec::new()));
    let feed = CaptureFeed::new(OfferingSource {
        inner: CountingSource {
            remaining: frames,
            produced: 0,
            resize_after: None,
            cursor: None,
            started: stream_started(),
        },
        offers: Arc::clone(&offers),
    });

    (feed, offers)
}

fn display(width: u32, height: u32, millihertz: u32) -> ControlMessage {
    ControlMessage::Display(
        ClientDisplay::new(
            Resolution::new(width, height),
            millihertz,
            DisplayVolume::srgb(),
        )
        .unwrap(),
    )
}

fn virtual_config() -> rusplay_config::Config {
    let mut config = host_config();
    config.capture.source = SourceKind::Virtual;
    config.capture.virtual_max_resolution = Resolution::new(3840, 2160);
    config.capture.virtual_max_refresh_hz = 144;

    config
}

#[test]
fn a_virtual_host_waits_for_the_display_before_answering_the_hello() {
    let fixture = Fixture::new(virtual_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(3);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let answered_early = client.pump(SHORT_WAIT, TestClient::has_hello);
    let offered_early = offers.lock().len();
    client.send(display(2560, 1440, 119_880));
    let answered = client.pump_until(TestClient::has_hello);

    assert!(!answered_early);
    assert_eq!(offered_early, 0);
    assert!(answered);
    assert_eq!(
        *offers.lock(),
        [OutputRequest {
            resolution: Resolution::new(2560, 1440),
            frame_rate: FrameRate::new(2997, 25),
        }]
    );
    assert_eq!(stream.virtual_output(), offers.lock().first().copied());
    stream.stop().unwrap();
}

#[test]
fn a_display_that_arrives_before_the_hello_is_kept() {
    let fixture = Fixture::new(virtual_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(3);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.send(display(1920, 1080, 60_000));
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let answered = client.pump_until(TestClient::has_hello);

    assert!(answered);
    assert_eq!(offers.lock().len(), 1);
    stream.stop().unwrap();
}

#[test]
fn a_hostile_display_is_held_to_the_configured_limits() {
    let fixture = Fixture::new(virtual_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(3);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.send(display(16_384, 16_384, 1_000_000));
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let answered = client.pump_until(TestClient::has_hello);

    assert!(answered);
    assert_eq!(
        *offers.lock(),
        [OutputRequest {
            resolution: Resolution::new(2160, 2160),
            frame_rate: FrameRate::integer(144),
        }]
    );
    stream.stop().unwrap();
}

#[test]
fn a_monitor_host_answers_the_hello_without_a_display_and_offers_nothing() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(3);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.send(display(2560, 1440, 60_000));
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let answered = client.pump_until(TestClient::has_hello);

    assert!(answered);
    assert!(offers.lock().is_empty());
    assert_eq!(stream.virtual_output(), None);
    stream.stop().unwrap();
}

fn resizing_config(interval_ms: u32) -> rusplay_config::Config {
    let mut config = virtual_config();
    config.capture.virtual_resize_min_interval_ms = interval_ms;

    config
}

fn offered_sizes(offers: &Mutex<Vec<OutputRequest>>) -> Vec<Resolution> {
    offers
        .lock()
        .iter()
        .map(|request| request.resolution)
        .collect()
}

#[test]
fn a_display_sent_while_streaming_resizes_the_offer() {
    let fixture = Fixture::new(resizing_config(10));
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(2_000);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.send(display(1280, 720, 60_000));
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));

    client.send(display(1600, 900, 60_000));
    let resized = client.pump_until(|_| offers.lock().len() == 2);
    let stats = stream.stats();

    assert!(resized);
    assert_eq!(
        offered_sizes(&offers),
        [Resolution::new(1280, 720), Resolution::new(1600, 900)]
    );
    assert_eq!(
        stream.virtual_output().map(|request| request.resolution),
        Some(Resolution::new(1600, 900))
    );
    assert_eq!(stats.display_updates_received, 1);
    assert_eq!(stats.virtual_resizes, 1);
    stream.stop().unwrap();
}

#[test]
fn a_repeated_display_for_the_size_in_force_offers_nothing_again() {
    let fixture = Fixture::new(resizing_config(10));
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(2_000);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.send(display(1280, 720, 60_000));
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));

    client.send(display(1280, 720, 60_000));
    let counted = client.pump_until(|_| stream.stats().display_updates_received == 1);
    client.pump(SHORT_WAIT, |_| false);

    assert!(counted);
    assert_eq!(offers.lock().len(), 1);
    assert_eq!(stream.stats().virtual_resizes, 0);
    stream.stop().unwrap();
}

#[test]
fn a_flood_of_displays_is_coalesced_to_one_offer_per_interval_and_ends_on_the_last() {
    const INTERVAL_MS: u32 = 300;
    const FLOOD: u32 = 150;

    let fixture = Fixture::new(resizing_config(INTERVAL_MS));
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(5_000);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.send(display(1280, 720, 60_000));
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));

    let started = Instant::now();
    for step in 0..FLOOD {
        client.send(display(1000 + step, 700, 60_000));
        client.pump(Duration::from_millis(2), |_| false);
    }
    let last = Resolution::new(1000 + FLOOD - 1, 700);
    let settled = client.pump_until(|_| offered_sizes(&offers).last() == Some(&last));
    let stats = stream.stats();
    let elapsed = started.elapsed();

    let allowed = 2 + usize::try_from(elapsed.as_millis() / u128::from(INTERVAL_MS)).unwrap();
    assert!(settled, "{:?}", offered_sizes(&offers));
    assert!(offers.lock().len() <= allowed, "{}", offers.lock().len());
    assert_eq!(stats.display_updates_received, u64::from(FLOOD));
    assert!(stats.display_updates_coalesced >= u64::from(FLOOD) - u64::try_from(allowed).unwrap());
    stream.stop().unwrap();
}

#[test]
fn a_display_below_the_minimum_is_offered_at_the_minimum() {
    let fixture = Fixture::new(resizing_config(10));
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(2_000);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.send(display(1280, 720, 60_000));
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));

    client.send(display(100, 80, 60_000));
    let resized = client.pump_until(|_| offers.lock().len() == 2);

    assert!(resized);
    assert_eq!(offered_sizes(&offers)[1], Resolution::new(250, 200));
    stream.stop().unwrap();
}

#[test]
fn a_monitor_host_ignores_a_display_sent_while_streaming() {
    let fixture = Fixture::new(host_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(2_000);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(client.pump_until(TestClient::has_hello));

    client.send(display(1600, 900, 60_000));
    client.pump(SHORT_WAIT, |_| false);

    assert!(offers.lock().is_empty());
    assert_eq!(stream.stats().display_updates_received, 0);
    stream.stop().unwrap();
}

#[test]
fn a_session_takeover_resizes_to_the_new_clients_display() {
    let fixture = Fixture::new(resizing_config(5_000));
    let bitrate = Arc::new(AtomicU32::new(0));
    let (feed, offers) = offering_feed(5_000);
    let serving = fixture.serve(Some(pin()), feed, fake_factory(&bitrate));

    let (mut first, record) = TestClient::pair(fixture.address);
    first.send(display(1280, 720, 60_000));
    first.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    assert!(first.pump_until(|client| client.has_hello() && client.frames.len() >= 2));

    let mut restarted = TestClient::reconnect(fixture.address, &record);
    restarted.hello();
    restarted.send(display(1000, 700, 60_000));
    let resized = restarted.pump_until(|_| offers.lock().len() == 2);

    assert!(resized);
    assert_eq!(
        offered_sizes(&offers),
        [Resolution::new(1280, 720), Resolution::new(1000, 700)]
    );
    assert_eq!(stream.stats().sessions_taken_over, 1);
    stream.stop().unwrap();
}
const OFFERED: Resolution = Resolution::new(1280, 720);
const STALE: Resolution = Resolution::new(64, 32);

struct ScriptedSource {
    sizes: VecDeque<Resolution>,
    produced: u32,
    started: Arc<AtomicBool>,
}

impl FrameSource for ScriptedSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        let ready = self.started.load(Ordering::Acquire);
        let Some(resolution) = self.sizes.front().copied().filter(|_| ready) else {
            thread::sleep(timeout);
            return Ok(None);
        };

        thread::sleep(Duration::from_millis(10).min(timeout));
        self.sizes.pop_front();
        self.produced += 1;
        let stride = resolution.width * 4;

        Ok(Some(Frame {
            resolution,
            format: DrmFourcc::from_code(*b"XR24"),
            color: ColorSpace::default(),
            timestamp: Timestamp::from_micros(u64::from(self.produced) * 10_000),
            buffer: FrameBuffer::Shm {
                data: vec![1; (stride * resolution.height) as usize].into(),
                stride,
            },
        }))
    }
}

fn scripted_feed(sizes: impl IntoIterator<Item = Resolution>) -> Arc<CaptureFeed> {
    CaptureFeed::new(ScriptedSource {
        sizes: sizes.into_iter().collect(),
        produced: 0,
        started: stream_started(),
    })
}

fn announced_sizes(client: &TestClient) -> Vec<Resolution> {
    client
        .formats
        .iter()
        .map(|(_, description)| description.resolution)
        .collect()
}

fn first_formats(timeout_ms: u32, sizes: Vec<Resolution>, wanted_frames: usize) -> Vec<Resolution> {
    let mut config = virtual_config();
    config.capture.virtual_first_frame_timeout_ms = timeout_ms;
    let fixture = Fixture::new(config);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), scripted_feed(sizes), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.send(display(OFFERED.width, OFFERED.height, 60_000));
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    client.pump_until(|client| client.frames.len() >= wanted_frames);
    stream.stop().unwrap();

    announced_sizes(&client)
}

#[test]
fn the_first_frame_is_held_until_one_of_the_offered_size_arrives() {
    let stale = std::iter::repeat_n(STALE, 8);
    let offered = std::iter::repeat_n(OFFERED, 8);

    let sizes = first_formats(300, stale.chain(offered).collect(), 4);

    assert_eq!(sizes, [OFFERED]);
}

#[test]
fn a_static_output_that_delivers_one_frame_at_the_offered_size_does_not_deadlock() {
    let sizes = first_formats(300, vec![STALE, OFFERED], 1);

    assert_eq!(sizes, [OFFERED]);
}

#[test]
fn a_compositor_that_ignores_the_offer_is_encoded_at_its_own_size_after_the_timeout() {
    let started = Instant::now();
    let sizes = first_formats(250, vec![STALE; 200], 3);

    assert_eq!(sizes, [STALE]);
    assert!(started.elapsed() >= Duration::from_millis(250));
}

#[test]
fn a_single_stale_frame_and_then_silence_is_encoded_when_the_timeout_passes() {
    let sizes = first_formats(250, vec![STALE], 1);

    assert_eq!(sizes, [STALE]);
}

#[test]
fn a_zero_timeout_turns_the_hold_off_and_the_stale_frame_goes_out_first() {
    let stale = std::iter::repeat_n(STALE, 4);
    let offered = std::iter::repeat_n(OFFERED, 40);

    let sizes = first_formats(0, stale.chain(offered).collect(), 30);

    assert_eq!(sizes.first(), Some(&STALE));
    assert_eq!(sizes.last(), Some(&OFFERED));
}

#[test]
fn a_monitor_host_never_holds_its_first_frame() {
    let mut config = host_config();
    config.capture.virtual_first_frame_timeout_ms = 10_000;
    let fixture = Fixture::new(config);
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(
        Some(pin()),
        scripted_feed(vec![STALE; 20]),
        fake_factory(&bitrate),
    );

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();
    let started = Instant::now();
    let arrived = client.pump_until(|client| !client.frames.is_empty());
    stream.stop().unwrap();

    assert!(arrived);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(announced_sizes(&client), [STALE]);
}
