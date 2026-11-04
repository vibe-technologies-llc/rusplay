use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use rusplay_capture::{CaptureError, Frame, OutputOffer, OutputRequest};
use rusplay_config::SourceKind;
use rusplay_core::{FrameRate, Resolution};
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
