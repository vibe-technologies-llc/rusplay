use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    time::Instant,
};

use rusplay_config::VideoDecoderConfig;
use rusplay_core::{Codec, ColorSpace};
use rusplay_decode::{DecodeError, DecodedFrame, VideoDecoder};

use crate::{
    ClientError,
    receive::{EncodedVideo, Feedback},
    slot::Slot,
    stats::Counters,
};

#[derive(Debug)]
pub(crate) struct DecodedVideo {
    pub decoded: DecodedFrame,
    pub number: u32,
    pub keyframe: bool,
    pub color: ColorSpace,
    pub received_at: Instant,
    pub decoded_at: Instant,
}

#[derive(Debug)]
pub(crate) struct DecodeLinks {
    pub queue: Receiver<EncodedVideo>,
    pub output: Arc<Slot<DecodedVideo>>,
    pub feedback: Sender<Feedback>,
    pub stop: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
}

struct CloseOnExit<'a>(&'a Slot<DecodedVideo>);

impl Drop for CloseOnExit<'_> {
    fn drop(&mut self) {
        drop(self.0.close());
    }
}

pub(crate) fn run(config: &VideoDecoderConfig, links: &DecodeLinks) -> Result<(), ClientError> {
    let _close = CloseOnExit(&links.output);

    let result = decode_loop(config, links);
    links.stop.store(true, Ordering::Relaxed);

    result
}

struct OpenDecoder {
    codec: Codec,
    decoder: Box<dyn VideoDecoder>,
}

fn decoder_for<'a>(
    slot: &'a mut Option<OpenDecoder>,
    codec: Codec,
    config: &VideoDecoderConfig,
) -> Result<&'a mut dyn VideoDecoder, DecodeError> {
    let open = match slot.take() {
        Some(open) if open.codec == codec => open,
        _ => OpenDecoder {
            codec,
            decoder: rusplay_decode::open(codec, config)?,
        },
    };

    Ok(slot.insert(open).decoder.as_mut())
}

fn decode_loop(config: &VideoDecoderConfig, links: &DecodeLinks) -> Result<(), ClientError> {
    let mut active = None;
    let mut skipping = false;

    while let Ok(video) = links.queue.recv() {
        if links.stop.load(Ordering::Relaxed) {
            break;
        }

        if video.frame.keyframe {
            skipping = false;
        } else if skipping {
            links.counters.frames_skipped.increment();
            continue;
        }

        let result = decoder_for(&mut active, video.stream.codec, config)?.decode(&video.frame);
        match result {
            Ok(Some(picture)) => {
                links.counters.frames_decoded.increment();
                let color = picture.color.over(video.stream.color);
                let output = DecodedVideo {
                    decoded: picture,
                    number: video.number,
                    keyframe: video.frame.keyframe,
                    color,
                    received_at: video.received_at,
                    decoded_at: Instant::now(),
                };
                match links.output.put(output) {
                    Ok(Some(superseded)) => {
                        links.counters.frames_superseded.increment();
                        drop(superseded);
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            Ok(None) => {}
            Err(error) if video.frame.keyframe => return Err(error.into()),
            Err(_) => {
                links.counters.decode_errors.increment();
                skipping = true;
                links
                    .feedback
                    .send(Feedback::DecodeFailed {
                        frame: video.number,
                    })
                    .ok();
            }
        }
    }

    Ok(())
}
