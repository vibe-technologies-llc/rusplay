use std::{sync::Arc, time::Duration};

use rusplay_capture::Frame;

use crate::{CaptureFeed, HostError, shared::Shared};

const CAPTURE_POLL: Duration = Duration::from_millis(20);

pub(crate) fn run(feed: &CaptureFeed, shared: &Arc<Shared>) -> Result<(), HostError> {
    let result = capture(feed, shared);
    shared.stop();

    result
}

fn capture(feed: &CaptureFeed, shared: &Shared) -> Result<(), HostError> {
    let mut source = feed.source();

    if let Some(frame) = feed.take_retained() {
        offer(shared, frame);
    }

    while !shared.stopped() {
        if let Some(frame) = source.next_frame(CAPTURE_POLL)? {
            shared.counters.frames_captured.increment();
            offer(shared, frame);
        }
    }

    Ok(())
}

fn offer(shared: &Shared, frame: Frame) {
    if let Ok(Some(_)) = shared.mailbox.put(frame) {
        shared.counters.frames_superseded.increment();
    }
}
