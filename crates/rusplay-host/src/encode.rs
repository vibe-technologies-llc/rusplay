use std::{
    collections::VecDeque,
    sync::{
        Arc,
        mpsc::{SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use rusplay_capture::OutputRequest;
use rusplay_core::{CodecFormat, EncodedFrame, FormatSet, FrameRate, Resolution, Timestamp};

use crate::{
    CaptureFeed, HostError,
    capture::Captured,
    mailbox::{Delivery, RecoveryRequest, Waited},
    negotiate::NegotiatedFormat,
    outbox::ControlOutbox,
    pipeline::{OutputRef, PipelineNotices, PipelineRecovery, StreamFormat, VideoPipeline},
    quality::{QualitySampler, QualitySettings, QualitySink},
    shared::Shared,
    stats::Counters,
};

const IDLE_WAKE: Duration = Duration::from_millis(20);
const REFERENCE_HISTORY: usize = 256;

pub(crate) trait PipelineFactory: Send {
    fn encodable(&mut self) -> Result<FormatSet, HostError>;

    fn use_output(&mut self, _request: OutputRequest) {}

    fn build(self: Box<Self>, format: CodecFormat) -> Result<Box<dyn VideoPipeline>, HostError>;
}

#[derive(Debug)]
pub(crate) struct Outgoing {
    pub number: u32,
    pub captured_micros: u64,
    pub recovers_from: Option<u32>,
    pub suspensions: u32,
    pub frame: EncodedFrame,
}

#[derive(Clone, Copy)]
struct Origin {
    captured_micros: u64,
    suspensions: u32,
    output: Option<OutputRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QualityTiming {
    pub interval_ms: u32,
    pub frame_rate: FrameRate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forwarded {
    Sent(u32),
    Dropped,
    Closed,
}

pub(crate) struct EncodeLinks {
    pub first_frame_timeout: Duration,
    pub quality: QualityTiming,
    pub shared: Arc<Shared>,
    pub feed: Arc<CaptureFeed>,
    pub output: SyncSender<Outgoing>,
    pub outbox: Arc<ControlOutbox>,
}

pub(crate) fn run(factory: Box<dyn PipelineFactory>, links: &EncodeLinks) -> Result<(), HostError> {
    let result = open_negotiated(factory, links).and_then(|opened| match opened {
        Some(mut opened) => {
            let output = links.shared.output();
            let rate = output.map_or(links.quality.frame_rate, |request| request.frame_rate);
            let sink: Arc<dyn QualitySink> = links.outbox.clone();
            let mut sampler =
                QualitySampler::new(QualitySettings::new(links.quality.interval_ms, rate), sink);
            EncodeStage::holding_first_frame(FirstFrameHold::new(
                output.map(|request| request.resolution),
                links.first_frame_timeout,
            ))
            .serve(
                &mut *opened.pipeline,
                opened.negotiated,
                &mut sampler,
                links,
            )
        }
        None => Ok(()),
    });
    links.shared.stop();

    result
}

struct NegotiatedPipeline {
    pipeline: Box<dyn VideoPipeline>,
    negotiated: NegotiatedFormat,
}

fn open_negotiated(
    mut factory: Box<dyn PipelineFactory>,
    links: &EncodeLinks,
) -> Result<Option<NegotiatedPipeline>, HostError> {
    let negotiation = &links.shared.negotiation;
    negotiation.offer(factory.encodable()?);

    loop {
        if links.shared.stopped() {
            return Ok(None);
        }
        if let Some(negotiated) = negotiation.wait_agreed(IDLE_WAKE) {
            if let Some(request) = links.shared.output() {
                factory.use_output(request);
            }
            return Ok(Some(NegotiatedPipeline {
                pipeline: factory.build(negotiated.chosen)?,
                negotiated,
            }));
        }
    }
}

fn record_notices(notices: PipelineNotices, counters: &Counters) {
    counters.dmabuf_fallbacks.add(notices.dmabuf_fallbacks);
    counters
        .dmabuf_import_failures
        .add(notices.dmabuf_import_failures);
    counters.handoff_disabled.add(notices.handoff_disabled);
}

#[derive(Debug, Default)]
struct References {
    sent: VecDeque<(u32, OutputRef)>,
}

impl References {
    fn record(&mut self, number: u32, output: OutputRef) {
        if self.sent.len() == REFERENCE_HISTORY {
            self.sent.pop_front();
        }
        self.sent.push_back((number, output));
    }

    fn output_of(&self, number: u32) -> Option<OutputRef> {
        self.sent
            .iter()
            .rev()
            .find(|(sent, _)| *sent == number)
            .map(|(_, output)| *output)
    }

    fn number_of(&self, output: OutputRef) -> Option<u32> {
        self.sent
            .iter()
            .rev()
            .find(|(_, sent)| *sent == output)
            .map(|(number, _)| *number)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    Encode,
    Wait,
}

#[derive(Debug, Clone, Copy)]
struct FirstFrameHold {
    wanted: Option<Resolution>,
    timeout: Duration,
    started: Option<Instant>,
    released: bool,
}

impl Default for FirstFrameHold {
    fn default() -> Self {
        Self::new(None, Duration::ZERO)
    }
}

impl FirstFrameHold {
    fn new(wanted: Option<Resolution>, timeout: Duration) -> Self {
        Self {
            wanted,
            timeout,
            started: None,
            released: wanted.is_none() || timeout.is_zero(),
        }
    }

    fn judge(&mut self, resolution: Resolution, now: Instant) -> Held {
        if self.released {
            return Held::Encode;
        }
        if self.wanted == Some(resolution) {
            self.released = true;
            return Held::Encode;
        }

        let started = *self.started.get_or_insert(now);
        if now.saturating_duration_since(started) >= self.timeout {
            self.released = true;
            Held::Encode
        } else {
            Held::Wait
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.started
            .filter(|_| !self.released)
            .map(|started| started + self.timeout)
    }
}

#[derive(Debug, Default)]
struct EncodeStage {
    hold: FirstFrameHold,
    held: Option<Captured>,
    next_number: u32,
    last_keyframe: Option<u32>,
    last_refresh: Option<u32>,
    force_keyframe: bool,
    recovering: Option<u32>,
    references: References,
    last_input: Option<(Timestamp, Instant)>,
    announced: Option<StreamFormat>,
    generation: u32,
}

impl EncodeStage {
    fn holding_first_frame(hold: FirstFrameHold) -> Self {
        Self {
            hold,
            ..Self::default()
        }
    }

    fn wait_timeout(&self, now: Instant) -> Duration {
        self.hold
            .deadline()
            .filter(|_| self.held.is_some())
            .map_or(IDLE_WAKE, |deadline| {
                deadline.saturating_duration_since(now).min(IDLE_WAKE)
            })
    }

    fn serve(
        &mut self,
        pipeline: &mut dyn VideoPipeline,
        negotiated: NegotiatedFormat,
        sampler: &mut QualitySampler,
        links: &EncodeLinks,
    ) -> Result<(), HostError> {
        let counters = &links.shared.counters;

        loop {
            let delivery = match links.shared.mailbox.wait(self.wait_timeout(Instant::now())) {
                Waited::Delivered(delivery) => delivery,
                Waited::Idle if links.shared.stopped() => return Ok(()),
                Waited::Idle if self.held.is_some() => {
                    Delivery::idle(links.shared.mailbox.suspensions())
                }
                Waited::Idle => continue,
                Waited::Closed => return Ok(()),
            };
            let Delivery {
                frame,
                keyframe_for,
                keyframe_forced,
                recovery_for,
                acknowledged,
                bitrate_kbps,
                suspensions,
            } = delivery;

            if let Some(kbps) = bitrate_kbps {
                pipeline.set_bitrate(kbps)?;
                counters.bitrate_changes.increment();
            }
            if keyframe_forced {
                self.force_keyframe = true;
            }
            if let Some(frame) = keyframe_for {
                counters.keyframe_requests.increment();
                if self.keyframe_follows(frame) {
                    counters.keyframe_requests_ignored.increment();
                } else {
                    self.force_keyframe = true;
                }
            }
            if let Some(frame) = acknowledged {
                self.acknowledge(pipeline, frame, counters);
            }
            if let Some(request) = recovery_for {
                self.recover(pipeline, request, counters);
            }

            if frame.is_some() {
                self.held = frame;
            }
            let frame = match self.held.take() {
                Some(captured) => {
                    match self.hold.judge(captured.frame.resolution, Instant::now()) {
                        Held::Encode => Some(captured),
                        Held::Wait => {
                            self.held = Some(captured);
                            continue;
                        }
                    }
                }
                None => None,
            };

            let (encoded, captured_micros) = match frame {
                Some(Captured {
                    frame,
                    captured_micros,
                }) => {
                    drop(links.feed.take_retained());
                    plan_sample(pipeline, sampler, links);
                    let encoded = pipeline.encode(&frame, self.force_keyframe);
                    record_notices(pipeline.take_notices(), counters);
                    let encoded = encoded?;
                    self.last_input = Some((frame.timestamp, Instant::now()));
                    links.feed.retain(frame);
                    (encoded, captured_micros)
                }
                None if self.force_keyframe || self.recovering.is_some() => {
                    counters.frames_repeated.increment();
                    sampler.interrupt();
                    let encoded = pipeline.repeat(self.repeat_timestamp(), self.force_keyframe);
                    record_notices(pipeline.take_notices(), counters);
                    let encoded = encoded?;
                    (encoded, links.shared.clock.micros())
                }
                None => continue,
            };

            let sample = pipeline.take_sample();
            let Some(encoded) = encoded else {
                sampler.record(sample, None, Instant::now());
                continue;
            };
            self.announce(pipeline.stream_format(), negotiated.configured, links)?;
            let origin = Origin {
                captured_micros,
                suspensions,
                output: pipeline.last_output(),
            };
            match self.forward(encoded, origin, links) {
                Forwarded::Sent(number) => sampler.record(sample, Some(number), Instant::now()),
                Forwarded::Dropped => sampler.record(sample, None, Instant::now()),
                Forwarded::Closed => return Ok(()),
            }
        }
    }

    fn announce(
        &mut self,
        format: Option<StreamFormat>,
        configured: CodecFormat,
        links: &EncodeLinks,
    ) -> Result<(), HostError> {
        let Some(format) = format.filter(|format| self.announced != Some(*format)) else {
            return Ok(());
        };

        self.generation = self.generation.wrapping_add(1);
        self.announced = Some(format);
        links
            .outbox
            .announce(format.describe(self.generation, configured))
    }

    fn acknowledge(&self, pipeline: &mut dyn VideoPipeline, frame: u32, counters: &Counters) {
        counters.frame_acks.increment();
        if let Some(output) = self.references.output_of(frame) {
            pipeline.acknowledge(output);
        }
    }

    fn recover(
        &mut self,
        pipeline: &mut dyn VideoPipeline,
        request: RecoveryRequest,
        counters: &Counters,
    ) {
        counters.recovery_requests.increment();
        if self.refresh_follows(request.frame) {
            counters.recovery_requests_ignored.increment();
            return;
        }
        if self.force_keyframe || self.recovering.is_some() {
            return;
        }

        let reference = self
            .references
            .output_of(request.last_good)
            .filter(|_| !self.keyframe_follows(request.last_good))
            .and_then(|last_good| match pipeline.recover(last_good) {
                PipelineRecovery::Reference(output) => self.references.number_of(output),
                PipelineRecovery::Keyframe => None,
            })
            .filter(|&number| request.last_good.wrapping_sub(number).cast_signed() >= 0);

        match reference {
            Some(number) => self.recovering = Some(number),
            None => self.force_keyframe = true,
        }
    }

    fn keyframe_follows(&self, frame: u32) -> bool {
        follows(self.last_keyframe, frame)
    }

    fn refresh_follows(&self, frame: u32) -> bool {
        follows(self.last_refresh, frame)
    }

    fn repeat_timestamp(&self) -> Timestamp {
        self.last_input.map_or(Timestamp::ZERO, |(timestamp, at)| {
            let elapsed = u64::try_from(at.elapsed().as_micros()).unwrap_or(u64::MAX);
            Timestamp::from_micros(timestamp.as_micros().saturating_add(elapsed))
        })
    }

    fn forward(&mut self, frame: EncodedFrame, origin: Origin, links: &EncodeLinks) -> Forwarded {
        let Origin {
            captured_micros,
            suspensions,
            output,
        } = origin;
        let number = self.next_number;
        let keyframe = frame.keyframe;
        let recovers_from = self.recovering.filter(|_| !keyframe);

        match links.output.try_send(Outgoing {
            number,
            captured_micros,
            recovers_from,
            suspensions,
            frame,
        }) {
            Ok(()) => {
                self.next_number = number.wrapping_add(1);
                self.force_keyframe = false;
                self.recovering = None;
                if keyframe {
                    self.last_keyframe = Some(number);
                }
                if keyframe || recovers_from.is_some() {
                    self.last_refresh = Some(number);
                }
                if recovers_from.is_some() {
                    links.shared.counters.reference_recoveries.increment();
                }
                if let Some(output) = output {
                    self.references.record(number, output);
                }
                links.shared.counters.frames_encoded.increment();
                Forwarded::Sent(number)
            }
            Err(TrySendError::Full(_)) => {
                links.shared.counters.frames_dropped.increment();
                self.force_keyframe = true;
                self.recovering = None;
                Forwarded::Dropped
            }
            Err(TrySendError::Disconnected(_)) => Forwarded::Closed,
        }
    }
}

fn plan_sample(
    pipeline: &mut dyn VideoPipeline,
    sampler: &mut QualitySampler,
    links: &EncodeLinks,
) {
    if let Some(kind) = sampler.plan(links.shared.quality_requested(), Instant::now()) {
        pipeline.sample_next(kind);
    }
}

fn follows(refresh: Option<u32>, frame: u32) -> bool {
    refresh.is_some_and(|refresh| refresh.wrapping_sub(frame).cast_signed() > 0)
}

#[cfg(test)]
mod tests {
    use rusplay_capture::{DrmFourcc, Frame, FrameBuffer};
    use rusplay_core::ColorSpace;
    use rusplay_encode::OutputIndex;

    use super::*;

    fn output(index: u64) -> OutputRef {
        OutputRef {
            epoch: 1,
            index: OutputIndex(index),
        }
    }

    const OFFERED: Resolution = Resolution::new(1280, 720);
    const STALE: Resolution = Resolution::new(1920, 1080);
    const TIMEOUT: Duration = Duration::from_millis(500);
    const MS: Duration = Duration::from_millis(1);

    fn captured(resolution: Resolution) -> Captured {
        Captured {
            frame: Frame {
                resolution,
                format: DrmFourcc::from_code(*b"XR24"),
                color: ColorSpace::default(),
                timestamp: Timestamp::ZERO,
                buffer: FrameBuffer::Shm {
                    data: vec![0; 4].into(),
                    stride: 4,
                },
            },
            captured_micros: 0,
        }
    }

    #[test]
    fn nothing_is_held_without_an_offered_size_or_with_a_zero_timeout() {
        let now = Instant::now();

        assert_eq!(FirstFrameHold::default().judge(STALE, now), Held::Encode);
        assert_eq!(
            FirstFrameHold::new(None, TIMEOUT).judge(STALE, now),
            Held::Encode
        );
        assert_eq!(
            FirstFrameHold::new(Some(OFFERED), Duration::ZERO).judge(STALE, now),
            Held::Encode
        );
    }

    #[test]
    fn a_frame_of_the_offered_size_releases_the_hold_at_once_and_for_good() {
        let now = Instant::now();
        let mut hold = FirstFrameHold::new(Some(OFFERED), TIMEOUT);

        let stale = hold.judge(STALE, now);
        let offered = hold.judge(OFFERED, now + 80 * MS);
        let later_stale = hold.judge(STALE, now + 90 * MS);

        assert_eq!(stale, Held::Wait);
        assert_eq!(offered, Held::Encode);
        assert_eq!(later_stale, Held::Encode);
        assert_eq!(hold.deadline(), None);
    }

    #[test]
    fn the_timeout_counts_from_the_first_stale_frame_and_then_releases_whatever_arrived() {
        let now = Instant::now();
        let mut hold = FirstFrameHold::new(Some(OFFERED), TIMEOUT);

        let first = hold.judge(STALE, now);
        let deadline = hold.deadline();
        let before = hold.judge(STALE, now + 499 * MS);
        let after = hold.judge(STALE, now + 500 * MS);

        assert_eq!(first, Held::Wait);
        assert_eq!(deadline, Some(now + TIMEOUT));
        assert_eq!(before, Held::Wait);
        assert_eq!(after, Held::Encode);
        assert_eq!(hold.deadline(), None);
    }

    #[test]
    fn the_stage_waits_no_longer_than_the_deadline_and_idles_normally_otherwise() {
        let now = Instant::now();
        let mut stage = EncodeStage::holding_first_frame(FirstFrameHold::new(
            Some(OFFERED),
            Duration::from_millis(5),
        ));

        let idle = stage.wait_timeout(now);
        stage.hold.judge(STALE, now);
        stage.held = Some(captured(STALE));
        let near = stage.wait_timeout(now);
        let overdue = stage.wait_timeout(now + 50 * MS);

        assert_eq!(idle, IDLE_WAKE);
        assert_eq!(near, Duration::from_millis(5));
        assert_eq!(overdue, Duration::ZERO);
    }
    #[test]
    fn a_request_for_a_frame_before_the_last_keyframe_is_already_answered() {
        let stage = EncodeStage {
            last_keyframe: Some(10),
            ..EncodeStage::default()
        };

        assert!(stage.keyframe_follows(9));
        assert!(!stage.keyframe_follows(10));
        assert!(!stage.keyframe_follows(11));
        assert!(!EncodeStage::default().keyframe_follows(0));
    }

    #[test]
    fn a_recovery_request_before_the_last_refresh_is_already_answered() {
        let stage = EncodeStage {
            last_refresh: Some(20),
            ..EncodeStage::default()
        };

        assert!(stage.refresh_follows(19));
        assert!(!stage.refresh_follows(20));
    }

    #[test]
    fn references_map_frame_numbers_to_outputs_both_ways() {
        let mut references = References::default();

        references.record(u32::MAX, output(7));
        references.record(0, output(8));

        assert_eq!(references.output_of(0), Some(output(8)));
        assert_eq!(references.number_of(output(7)), Some(u32::MAX));
        assert_eq!(references.output_of(1), None);
    }

    #[test]
    fn the_reference_history_forgets_the_oldest_frame() {
        let mut references = References::default();

        for number in 0..=u32::try_from(REFERENCE_HISTORY).unwrap() {
            references.record(number, output(u64::from(number)));
        }

        assert_eq!(references.output_of(0), None);
        assert_eq!(references.output_of(1), Some(output(1)));
    }
}
