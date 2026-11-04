use std::{
    collections::VecDeque,
    sync::{
        Arc,
        mpsc::{SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use rusplay_capture::OutputRequest;
use rusplay_core::{CodecFormat, EncodedFrame, FormatSet, Timestamp};

use crate::{
    CaptureFeed, HostError,
    capture::Captured,
    mailbox::{Delivery, RecoveryRequest, Waited},
    negotiate::NegotiatedFormat,
    outbox::ControlOutbox,
    pipeline::{OutputRef, PipelineNotices, PipelineRecovery, StreamFormat, VideoPipeline},
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

pub(crate) struct EncodeLinks {
    pub shared: Arc<Shared>,
    pub feed: Arc<CaptureFeed>,
    pub output: SyncSender<Outgoing>,
    pub outbox: Arc<ControlOutbox>,
}

pub(crate) fn run(factory: Box<dyn PipelineFactory>, links: &EncodeLinks) -> Result<(), HostError> {
    let result = open_negotiated(factory, links).and_then(|opened| match opened {
        Some(mut opened) => {
            EncodeStage::default().serve(&mut *opened.pipeline, opened.negotiated, links)
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

#[derive(Debug, Default)]
struct EncodeStage {
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
    fn serve(
        &mut self,
        pipeline: &mut dyn VideoPipeline,
        negotiated: NegotiatedFormat,
        links: &EncodeLinks,
    ) -> Result<(), HostError> {
        let counters = &links.shared.counters;

        loop {
            let delivery = match links.shared.mailbox.wait(IDLE_WAKE) {
                Waited::Delivered(delivery) => delivery,
                Waited::Idle if !links.shared.stopped() => continue,
                Waited::Idle | Waited::Closed => return Ok(()),
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

            let (encoded, captured_micros) = match frame {
                Some(Captured {
                    frame,
                    captured_micros,
                }) => {
                    drop(links.feed.take_retained());
                    let encoded = pipeline.encode(&frame, self.force_keyframe);
                    record_notices(pipeline.take_notices(), counters);
                    let encoded = encoded?;
                    self.last_input = Some((frame.timestamp, Instant::now()));
                    links.feed.retain(frame);
                    (encoded, captured_micros)
                }
                None if self.force_keyframe || self.recovering.is_some() => {
                    counters.frames_repeated.increment();
                    let encoded = pipeline.repeat(self.repeat_timestamp(), self.force_keyframe);
                    record_notices(pipeline.take_notices(), counters);
                    let encoded = encoded?;
                    (encoded, links.shared.clock.micros())
                }
                None => continue,
            };

            let Some(encoded) = encoded else {
                continue;
            };
            self.announce(pipeline.stream_format(), negotiated.configured, links)?;
            let origin = Origin {
                captured_micros,
                suspensions,
                output: pipeline.last_output(),
            };
            if !self.forward(encoded, origin, links) {
                return Ok(());
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

    fn forward(&mut self, frame: EncodedFrame, origin: Origin, links: &EncodeLinks) -> bool {
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
                true
            }
            Err(TrySendError::Full(_)) => {
                links.shared.counters.frames_dropped.increment();
                self.force_keyframe = true;
                self.recovering = None;
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

fn follows(refresh: Option<u32>, frame: u32) -> bool {
    refresh.is_some_and(|refresh| refresh.wrapping_sub(frame).cast_signed() > 0)
}

#[cfg(test)]
mod tests {
    use rusplay_encode::OutputIndex;

    use super::*;

    fn output(index: u64) -> OutputRef {
        OutputRef {
            epoch: 1,
            index: OutputIndex(index),
        }
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
