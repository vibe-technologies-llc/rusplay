use std::{
    sync::{
        Arc,
        mpsc::{SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use rusplay_core::{EncodedFrame, Timestamp};

use crate::{
    CaptureFeed, HostError,
    mailbox::{Delivery, Waited},
    pipeline::VideoPipeline,
    shared::Shared,
};

const IDLE_WAKE: Duration = Duration::from_millis(20);

pub(crate) type PipelineFactory =
    Box<dyn FnOnce() -> Result<Box<dyn VideoPipeline>, HostError> + Send>;

#[derive(Debug)]
pub(crate) struct Outgoing {
    pub number: u32,
    pub frame: EncodedFrame,
}

pub(crate) struct EncodeLinks {
    pub shared: Arc<Shared>,
    pub feed: Arc<CaptureFeed>,
    pub output: SyncSender<Outgoing>,
}

pub(crate) fn run(factory: PipelineFactory, links: &EncodeLinks) -> Result<(), HostError> {
    let result =
        factory().and_then(|mut pipeline| EncodeStage::default().serve(&mut *pipeline, links));
    links.shared.stop();

    result
}

#[derive(Debug, Default)]
struct EncodeStage {
    next_number: u32,
    last_keyframe: Option<u32>,
    force_keyframe: bool,
    last_input: Option<(Timestamp, Instant)>,
}

impl EncodeStage {
    fn serve(
        &mut self,
        pipeline: &mut dyn VideoPipeline,
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

            if let Some(encoded) = encoded
                && !self.forward(encoded, links)
            {
                return Ok(());
            }
        }
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
