use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    time::{Duration, Instant},
};

use rusplay_config::VideoDecoderConfig;
use rusplay_core::{ChromaSiting, Codec, ColorSpace};
use rusplay_decode::{DecodeError, DecodedFrame, VideoDecoder};

use crate::{
    ClientError,
    receive::{DecodeInput, Feedback},
    slot::Slot,
    stats::Counters,
};

const STOP_POLL: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub(crate) struct DecodedVideo {
    pub decoded: DecodedFrame,
    pub number: u32,
    pub keyframe: bool,
    pub color: ColorSpace,
    pub siting: Option<ChromaSiting>,
    pub captured_micros: u64,
    pub received_at: Instant,
    pub decoded_at: Instant,
}

#[derive(Debug)]
pub(crate) struct DecodeLinks {
    pub queue: Receiver<DecodeInput>,
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
    let mut last_decoded: Option<u32> = None;

    loop {
        let input = match links.queue.recv_timeout(STOP_POLL) {
            Ok(input) => input,
            Err(RecvTimeoutError::Timeout) => {
                if links.stop.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };

        if links.stop.load(Ordering::Relaxed) {
            break;
        }

        let video = match input {
            DecodeInput::Open(codec) => {
                decoder_for(&mut active, codec, config)?;
                last_decoded = None;
                continue;
            }
            DecodeInput::Video(video) => video,
        };

        if video.frame.keyframe || resumes(video.recovers_from, last_decoded) {
            skipping = false;
        } else if skipping {
            links.counters.frames_skipped.increment();
            continue;
        }

        let result = decoder_for(&mut active, video.stream.codec, config)?.decode(&video.frame);
        if result.is_ok() {
            last_decoded = Some(video.number);
            links
                .feedback
                .send(Feedback::Decoded {
                    frame: video.number,
                })
                .ok();
        }
        match result {
            Ok(Some(picture)) => {
                links.counters.frames_decoded.increment();
                let color = picture.color.over(video.stream.color);
                let siting = Some(resolve_siting(
                    picture.color.chroma_siting,
                    video.stream.siting,
                ));
                let decoded_at = Instant::now();
                if let Some(latency) = links
                    .counters
                    .clock
                    .since_capture(video.captured_micros, decoded_at)
                {
                    links.counters.capture_to_decode.record(latency);
                }
                let output = DecodedVideo {
                    decoded: picture,
                    number: video.number,
                    keyframe: video.frame.keyframe,
                    color,
                    siting,
                    captured_micros: video.captured_micros,
                    received_at: video.received_at,
                    decoded_at,
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

fn resumes(recovers_from: Option<u32>, last_decoded: Option<u32>) -> bool {
    recovers_from
        .zip(last_decoded)
        .is_some_and(|(reference, decoded)| decoded.wrapping_sub(reference).cast_signed() >= 0)
}

const fn resolve_siting(bitstream: Option<ChromaSiting>, described: ChromaSiting) -> ChromaSiting {
    match bitstream {
        Some(siting) => siting,
        None => described,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use rusplay_config::SoftwareDecoderConfig;

    use super::*;

    #[test]
    fn a_recovery_frame_resumes_only_from_a_decoded_reference() {
        assert!(resumes(Some(4), Some(4)));
        assert!(resumes(Some(4), Some(9)));
        assert!(resumes(Some(u32::MAX), Some(1)));
        assert!(!resumes(Some(5), Some(4)));
        assert!(!resumes(Some(4), None));
        assert!(!resumes(None, Some(4)));
    }

    #[test]
    fn the_bitstream_siting_wins_over_the_described_one() {
        for bitstream in [ChromaSiting::Left, ChromaSiting::Center] {
            for described in [ChromaSiting::Left, ChromaSiting::Center] {
                assert_eq!(resolve_siting(Some(bitstream), described), bitstream);
            }
        }
    }

    #[test]
    fn the_described_siting_covers_a_bitstream_that_signals_none() {
        for described in [ChromaSiting::Left, ChromaSiting::Center] {
            assert_eq!(resolve_siting(None, described), described);
        }
    }

    #[test]
    fn a_decoder_open_request_opens_the_decoder_before_any_frame() {
        let (queue, queued) = mpsc::sync_channel(4);
        let (feedback, _feedback_received) = mpsc::channel();
        let links = DecodeLinks {
            queue: queued,
            output: Arc::new(Slot::new()),
            feedback,
            stop: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(Counters::default()),
        };
        let config = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());

        queue.send(DecodeInput::Open(Codec::H264)).unwrap();
        drop(queue);
        let result = run(&config, &links);

        if cfg!(feature = "ffmpeg") {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(ClientError::Decode(_))));
        }
        assert_eq!(links.counters.snapshot().frames_decoded, 0);
    }

    #[test]
    fn the_stop_flag_ends_decode_while_the_queue_sender_is_alive() {
        let (queue, queued) = mpsc::sync_channel(4);
        let (feedback, _feedback_received) = mpsc::channel();
        let output = Arc::new(Slot::new());
        let links = DecodeLinks {
            queue: queued,
            output: Arc::clone(&output),
            feedback,
            stop: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(Counters::default()),
        };
        let stop = Arc::clone(&links.stop);
        let config = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());
        let (finished, finished_received) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            finished.send(run(&config, &links)).ok();
        });
        stop.store(true, Ordering::Relaxed);

        let result = finished_received.recv_timeout(Duration::from_secs(10));
        worker.join().unwrap();

        assert!(matches!(result, Ok(Ok(()))));
        assert!(output.take().is_none());
        drop(queue);
    }
}
