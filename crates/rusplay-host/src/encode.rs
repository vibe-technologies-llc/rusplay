use std::{
    sync::{
        Arc,
        mpsc::{SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use rusplay_core::{CodecFormat, EncodedFrame, FormatSet, Timestamp};

use crate::{
    CaptureFeed, HostError,
    mailbox::{Delivery, Waited},
    negotiate::NegotiatedFormat,
    outbox::ControlOutbox,
    pipeline::{StreamFormat, VideoPipeline},
    shared::Shared,
};

const IDLE_WAKE: Duration = Duration::from_millis(20);

pub(crate) trait PipelineFactory: Send {
    fn encodable(&mut self) -> Result<FormatSet, HostError>;

    fn build(self: Box<Self>, format: CodecFormat) -> Result<Box<dyn VideoPipeline>, HostError>;
}

#[derive(Debug)]
pub(crate) struct Outgoing {
    pub number: u32,
    pub frame: EncodedFrame,
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
            return Ok(Some(NegotiatedPipeline {
                pipeline: factory.build(negotiated.chosen)?,
                negotiated,
            }));
        }
    }
}

#[derive(Debug, Default)]
struct EncodeStage {
    next_number: u32,
    last_keyframe: Option<u32>,
    force_keyframe: bool,
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
                bitrate_kbps,
            } = delivery;

            if let Some(kbps) = bitrate_kbps {
                pipeline.set_bitrate(kbps)?;
                counters.bitrate_changes.increment();
            }
            if let Some(frame) = keyframe_for {
                counters.keyframe_requests.increment();
                if self.keyframe_follows(frame) {
                    counters.keyframe_requests_ignored.increment();
                } else {
                    self.force_keyframe = true;
                }
            }

            let encoded = match frame {
                Some(frame) => {
                    drop(links.feed.take_retained());
                    let encoded = pipeline.encode(&frame, self.force_keyframe)?;
                    self.last_input = Some((frame.timestamp, Instant::now()));
                    links.feed.retain(frame);
                    encoded
                }
                None if self.force_keyframe => {
                    counters.frames_repeated.increment();
                    pipeline.repeat(self.repeat_timestamp(), true)?
                }
                None => continue,
            };
            self.force_keyframe = false;

            let Some(encoded) = encoded else {
                continue;
            };
            self.announce(pipeline.stream_format(), negotiated.configured, links)?;
            if !self.forward(encoded, links) {
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

    fn keyframe_follows(&self, frame: u32) -> bool {
        self.last_keyframe
            .is_some_and(|keyframe| keyframe.wrapping_sub(frame).cast_signed() > 0)
    }

    fn repeat_timestamp(&self) -> Timestamp {
        self.last_input.map_or(Timestamp::ZERO, |(timestamp, at)| {
            let elapsed = u64::try_from(at.elapsed().as_micros()).unwrap_or(u64::MAX);
            Timestamp::from_micros(timestamp.as_micros().saturating_add(elapsed))
        })
    }

    fn forward(&mut self, frame: EncodedFrame, links: &EncodeLinks) -> bool {
        let number = self.next_number;
        let keyframe = frame.keyframe;

        match links.output.try_send(Outgoing { number, frame }) {
            Ok(()) => {
                self.next_number = number.wrapping_add(1);
                if keyframe {
                    self.last_keyframe = Some(number);
                }
                links.shared.counters.frames_encoded.increment();
                true
            }
            Err(TrySendError::Full(_)) => {
                links.shared.counters.frames_dropped.increment();
                self.force_keyframe = true;
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
