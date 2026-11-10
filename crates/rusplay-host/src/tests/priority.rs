use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_capture::{CaptureError, Frame};
use rusplay_input::{InputError, InputEvent, PadFeedback};

use super::{
    AtomicU32, Fixture, POLL, TEST_NICE, TestClient, WAIT, current_thread_nice, fake_factory, feed,
    host_config, network_with_nice, pin,
};
use crate::{CaptureFeed, FrameSource, InputFeed, InputTarget, tuning::TuningRefusals};

type ObservedNice = Arc<Mutex<Option<i32>>>;

struct NiceSource {
    nice: ObservedNice,
}

impl FrameSource for NiceSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        self.nice.lock().get_or_insert_with(current_thread_nice);
        thread::sleep(timeout);

        Ok(None)
    }
}

struct NiceTarget {
    nice: ObservedNice,
}

impl InputTarget for NiceTarget {
    fn inject(&mut self, _event: InputEvent) -> Result<(), InputError> {
        Ok(())
    }

    fn service(&mut self, _feedback: &mut Vec<PadFeedback>) -> Result<(), InputError> {
        self.nice.lock().get_or_insert_with(current_thread_nice);

        Ok(())
    }
}

fn observed(nice: &ObservedNice) -> Option<i32> {
    let deadline = Instant::now() + WAIT;
    while nice.lock().is_none() && Instant::now() < deadline {
        thread::sleep(POLL);
    }

    *nice.lock()
}

fn tuned_config() -> rusplay_config::Config {
    let mut config = host_config();
    config.network = network_with_nice(TEST_NICE);

    config
}

#[test]
fn the_capture_and_input_stage_threads_take_the_configured_nice_value() {
    let capture_nice = ObservedNice::default();
    let input_nice = ObservedNice::default();
    let target = NiceTarget {
        nice: Arc::clone(&input_nice),
    };
    let source = NiceSource {
        nice: Arc::clone(&capture_nice),
    };
    let fixture = Fixture::with_session(tuned_config(), move |session| {
        session.without_audio().with_input(InputFeed::new(target))
    });
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(
        Some(pin()),
        CaptureFeed::new(source),
        fake_factory(&bitrate),
    );

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();

    assert_eq!(observed(&capture_nice), Some(i32::from(TEST_NICE)));
    assert_eq!(observed(&input_nice), Some(i32::from(TEST_NICE)));
    assert_eq!(stream.stats().thread_priority_failures, 0);
    stream.stop().unwrap();
}

#[test]
fn a_session_counts_what_the_capture_loop_and_the_injector_were_refused() {
    const CAPTURE_LOOP_REFUSALS: u64 = 2;
    const INJECTOR_REFUSALS: u64 = 3;

    let target = NiceTarget {
        nice: ObservedNice::default(),
    };
    let fixture = Fixture::with_session(tuned_config(), move |session| {
        session.without_audio().with_input(InputFeed::tuned(
            target,
            TuningRefusals::of(INJECTOR_REFUSALS),
        ))
    });
    let source = NiceSource {
        nice: ObservedNice::default(),
    };
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(
        Some(pin()),
        CaptureFeed::tuned(source, TuningRefusals::of(CAPTURE_LOOP_REFUSALS)),
        fake_factory(&bitrate),
    );

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();

    assert_eq!(
        stream.stats().thread_priority_failures,
        CAPTURE_LOOP_REFUSALS + INJECTOR_REFUSALS
    );
    stream.stop().unwrap();
}

#[test]
fn a_session_without_standing_threads_counts_no_refusals() {
    let fixture = Fixture::new(tuned_config());
    let bitrate = Arc::new(AtomicU32::new(0));
    let serving = fixture.serve(Some(pin()), feed(1), fake_factory(&bitrate));

    let (mut client, _) = TestClient::pair(fixture.address);
    client.hello();
    let (_, stream) = serving.join().unwrap().unwrap();

    assert_eq!(stream.stats().thread_priority_failures, 0);
    stream.stop().unwrap();
}
