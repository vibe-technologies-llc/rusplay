use std::{thread, time::Duration};

use parking_lot::Mutex;
use rusplay_core::{BitDepth, ChromaSubsampling, PixelFormat};

use super::*;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<QualityEvent>>,
}

impl Recorder {
    fn take_events(&self) -> Vec<QualityEvent> {
        std::mem::take(&mut *self.events.lock())
    }
}

impl QualitySink for Recorder {
    fn deliver(&self, event: QualityEvent) {
        self.events.lock().push(event);
    }
}

const INTERVAL: Duration = Duration::from_millis(1_000);
const WIDTH: usize = 16;
const HEIGHT: usize = 8;

fn format() -> PixelFormat {
    PixelFormat {
        chroma: ChromaSubsampling::Yuv444,
        depth: BitDepth::Eight,
    }
}

fn plane(value: u16) -> SamplePlane {
    SamplePlane::new(WIDTH, HEIGHT, vec![value; WIDTH * HEIGHT]).unwrap()
}

fn picture(value: u16) -> SamplePicture {
    SamplePicture::new(format(), [plane(value), plane(128), plane(128)]).unwrap()
}

fn sampler(interval_ms: u32, fps: u32) -> (QualitySampler, Arc<Recorder>) {
    let link = Arc::new(Recorder::default());
    let sink: Arc<dyn QualitySink> = link.clone();
    let settings = QualitySettings::new(interval_ms, FrameRate::new(fps, 1));

    (QualitySampler::new(settings, sink), link)
}

fn wait_for_events(link: &Recorder) -> Vec<QualityEvent> {
    for _ in 0..500 {
        let events = link.take_events();
        if !events.is_empty() {
            return events;
        }
        thread::sleep(Duration::from_millis(2));
    }

    Vec::new()
}

#[test]
fn nothing_is_sampled_until_the_client_asks() {
    let (mut sampler, link) = sampler(1_000, 60);
    let now = Instant::now();

    assert_eq!(sampler.plan(false, now), None);
    assert_eq!(link.take_events(), []);
}

#[test]
fn a_second_order_rate_collects_two_earlier_lumas_before_the_measured_frame() {
    let (mut sampler, link) = sampler(1_000, 60);
    let now = Instant::now();

    let first = sampler.plan(true, now);
    sampler.record(Some(Sample::Luma(plane(10))), Some(1), now);
    let second = sampler.plan(true, now);
    sampler.record(Some(Sample::Luma(plane(12))), Some(2), now);
    let third = sampler.plan(true, now);
    sampler.record(
        Some(Sample::Picture(Box::new(PictureSample {
            source: picture(14),
            reconstruction: Some(picture(15)),
        }))),
        Some(3),
        now,
    );
    let events = wait_for_events(&link);

    assert_eq!(first, Some(SampleKind::Luma));
    assert_eq!(second, Some(SampleKind::Luma));
    assert_eq!(third, Some(SampleKind::Picture));
    assert!(matches!(
        events.as_slice(),
        [QualityEvent::Report { frame: 3, score }] if score.psnr.luma.value() > 40.0
    ));
}

#[test]
fn a_first_order_rate_collects_one_earlier_luma() {
    let (mut sampler, _link) = sampler(1_000, 30);
    let now = Instant::now();

    let first = sampler.plan(true, now);
    sampler.record(Some(Sample::Luma(plane(10))), Some(1), now);
    let second = sampler.plan(true, now);

    assert_eq!(first, Some(SampleKind::Luma));
    assert_eq!(second, Some(SampleKind::Picture));
}

#[test]
fn the_next_sample_waits_for_the_interval() {
    let (mut sampler, link) = sampler(1_000, 30);
    let now = Instant::now();

    sampler.plan(true, now);
    sampler.record(Some(Sample::Luma(plane(10))), Some(1), now);
    sampler.plan(true, now);
    sampler.record(
        Some(Sample::Picture(Box::new(PictureSample {
            source: picture(10),
            reconstruction: Some(picture(10)),
        }))),
        Some(2),
        now,
    );
    let early = sampler.plan(true, now + INTERVAL / 2);
    let due = sampler.plan(true, now + INTERVAL);
    let events = wait_for_events(&link);

    assert_eq!(early, None);
    assert_eq!(due, Some(SampleKind::Luma));
    assert!(matches!(
        events.as_slice(),
        [QualityEvent::Report { frame: 2, score }] if score.psnr.luma.is_identical()
    ));
}

#[test]
fn a_dropped_or_repeated_frame_restarts_the_history() {
    let (mut sampler, _link) = sampler(1_000, 30);
    let now = Instant::now();

    sampler.plan(true, now);
    sampler.record(Some(Sample::Luma(plane(10))), None, now);
    let after_drop = sampler.plan(true, now);
    sampler.record(Some(Sample::Luma(plane(10))), Some(1), now);
    sampler.interrupt();
    let after_repeat = sampler.plan(true, now);

    assert_eq!(after_drop, Some(SampleKind::Luma));
    assert_eq!(after_repeat, Some(SampleKind::Luma));
}

#[test]
fn an_encoder_without_reconstruction_is_reported_once_and_never_sampled_again() {
    let (mut sampler, link) = sampler(1_000, 30);
    let now = Instant::now();

    sampler.plan(true, now);
    sampler.record(Some(Sample::Luma(plane(10))), Some(1), now);
    sampler.plan(true, now);
    sampler.record(Some(Sample::Unsupported), Some(2), now);
    let later = sampler.plan(true, now + 10 * INTERVAL);

    assert_eq!(later, None);
    assert_eq!(
        link.take_events(),
        [QualityEvent::Unavailable(
            QualityUnavailableReason::EncoderUnsupported
        )]
    );
}

#[test]
fn a_host_with_sampling_off_refuses_once_per_request() {
    let (mut sampler, link) = sampler(0, 60);
    let now = Instant::now();

    let first = sampler.plan(true, now);
    let second = sampler.plan(true, now);
    let refused_once = link.take_events();
    sampler.plan(false, now);
    sampler.plan(true, now);

    assert_eq!(first, None);
    assert_eq!(second, None);
    assert_eq!(
        refused_once,
        [QualityEvent::Unavailable(
            QualityUnavailableReason::HostDisabled
        )]
    );
    assert_eq!(
        link.take_events(),
        [QualityEvent::Unavailable(
            QualityUnavailableReason::HostDisabled
        )]
    );
}

#[test]
fn a_failed_read_is_reported_and_waits_for_the_interval() {
    let (mut sampler, link) = sampler(1_000, 30);
    let now = Instant::now();

    sampler.plan(true, now);
    sampler.record(Some(Sample::Failed), Some(1), now);
    let early = sampler.plan(true, now);

    assert_eq!(early, None);
    assert_eq!(
        link.take_events(),
        [QualityEvent::Unavailable(
            QualityUnavailableReason::MeasurementFailed
        )]
    );
}

#[test]
fn a_history_of_another_size_is_not_measured() {
    let (mut sampler, link) = sampler(1_000, 30);
    let now = Instant::now();
    let small = SamplePlane::new(4, 4, vec![0; 16]).unwrap();

    sampler.plan(true, now);
    sampler.record(Some(Sample::Luma(small)), Some(1), now);
    sampler.plan(true, now);
    sampler.record(
        Some(Sample::Picture(Box::new(PictureSample {
            source: picture(10),
            reconstruction: Some(picture(10)),
        }))),
        Some(2),
        now,
    );
    let again = sampler.plan(true, now);

    assert_eq!(again, Some(SampleKind::Luma));
    assert_eq!(link.take_events(), []);
}
