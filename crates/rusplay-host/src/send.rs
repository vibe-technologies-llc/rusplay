use std::{
    sync::{
        Arc,
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use rusplay_net::{FecScheme, Pacer, Packetizer, Transport};
use rusplay_protocol::{StreamKind, VideoFrameHeader};

use crate::{HostError, encode::Outgoing, shared::Shared};

const IDLE_WAKE: Duration = Duration::from_millis(20);
const MIN_BURST_GAP: Duration = Duration::from_micros(250);
const MICROS_PER_MILLI: u128 = 1_000;
const BITS_PER_BYTE: u128 = 8;
const PERCENT: u32 = 100;
const CADENCE_SMOOTHING: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pacing {
    pub frame_interval: Duration,
    pub window_percent: u8,
}

impl Pacing {
    fn window(self, measured: Option<Duration>) -> Duration {
        let interval = measured.map_or(self.frame_interval, |measured| {
            measured.min(self.frame_interval)
        });

        interval * u32::from(self.window_percent) / PERCENT
    }
}

#[derive(Debug, Default)]
struct FrameCadence {
    last_captured_micros: Option<u64>,
    smoothed: Option<Duration>,
}

impl FrameCadence {
    fn observe(&mut self, captured_micros: u64) {
        let gap = self
            .last_captured_micros
            .replace(captured_micros)
            .and_then(|last| captured_micros.checked_sub(last))
            .filter(|gap| *gap > 0)
            .map(Duration::from_micros);
        let Some(gap) = gap else {
            return;
        };

        self.smoothed = Some(self.smoothed.map_or(gap, |smoothed| {
            (smoothed * (CADENCE_SMOOTHING - 1) + gap) / CADENCE_SMOOTHING
        }));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SendOptions {
    pub fec: FecScheme,
    pub pacing: Option<Pacing>,
}

pub(crate) struct SendLinks {
    pub shared: Arc<Shared>,
    pub queue: Receiver<Outgoing>,
    pub transport: Arc<Transport>,
}

pub(crate) fn run(options: SendOptions, links: &SendLinks) -> Result<(), HostError> {
    let result = serve(options, links);
    links.shared.stop();

    result
}

fn serve(options: SendOptions, links: &SendLinks) -> Result<(), HostError> {
    let mut packetizer = Packetizer::new(
        StreamKind::Video,
        links
            .transport
            .max_datagram_len(&links.shared.client.address()),
        options.fec,
    )?;

    let mut payload = Vec::new();
    let mut cadence = FrameCadence::default();

    while !links.shared.stopped() {
        match links.queue.recv_timeout(IDLE_WAKE) {
            Ok(outgoing) => {
                cadence.observe(outgoing.captured_micros);
                let window = options.pacing.map(|pacing| pacing.window(cadence.smoothed));
                send_frame(&mut packetizer, &mut payload, window, &outgoing, links)?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

fn send_frame(
    packetizer: &mut Packetizer,
    payload: &mut Vec<u8>,
    window: Option<Duration>,
    outgoing: &Outgoing,
    links: &SendLinks,
) -> Result<(), HostError> {
    payload.clear();
    VideoFrameHeader {
        captured_micros: outgoing.captured_micros,
    }
    .write(payload);
    payload.extend_from_slice(&outgoing.frame.data);
    let datagrams = packetizer.packetize(outgoing.number, outgoing.frame.keyframe, payload)?;

    match window {
        Some(window) => send_paced(&datagrams, window, links)?,
        None => send_all(&datagrams, links)?,
    }

    let counters = &links.shared.counters;
    counters.frames_sent.increment();
    counters.datagrams_sent.add(datagrams.len() as u64);
    if outgoing.frame.keyframe {
        counters.keyframes_sent.increment();
    }

    Ok(())
}

fn send_paced(datagrams: &[Vec<u8>], window: Duration, links: &SendLinks) -> Result<(), HostError> {
    let total: usize = datagrams.iter().map(Vec::len).sum();
    let plan = BurstPlan::new(datagrams.len(), total, window);
    let started = Instant::now();
    let mut sent_bytes = 0;

    for (index, burst) in datagrams.chunks(plan.datagrams_per_burst).enumerate() {
        if index > 0 {
            let due = started + plan.pacer.interval_for(sent_bytes);
            let wait = due.saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                thread::sleep(wait);
            }
        }

        send_all(burst, links)?;
        sent_bytes += burst.iter().map(Vec::len).sum::<usize>();
    }

    Ok(())
}

fn send_all(datagrams: &[Vec<u8>], links: &SendLinks) -> Result<(), HostError> {
    let mut sent = 0;

    while sent < datagrams.len() {
        let progress = links
            .transport
            .send_batch(&datagrams[sent..], links.shared.client.address())?;
        if progress == 0 {
            thread::yield_now();
        }
        sent += progress;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BurstPlan {
    datagrams_per_burst: usize,
    pacer: Pacer,
}

impl BurstPlan {
    fn new(datagrams: usize, bytes: usize, window: Duration) -> Self {
        let bursts = usize::try_from(window.as_nanos() / MIN_BURST_GAP.as_nanos())
            .unwrap_or(usize::MAX)
            .clamp(1, datagrams.max(1));
        let window_micros = window.as_micros().max(1);
        let kbps = (bytes as u128 * BITS_PER_BYTE * MICROS_PER_MILLI).div_ceil(window_micros);

        Self {
            datagrams_per_burst: datagrams.div_ceil(bursts).max(1),
            pacer: Pacer::from_kbps(u32::try_from(kbps).unwrap_or(u32::MAX).max(1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_spread_across_the_pacing_window() {
        let window = Duration::from_millis(8);

        let plan = BurstPlan::new(100, 100_000, window);

        assert_eq!(plan.datagrams_per_burst, 4);
        assert!(plan.pacer.interval_for(100_000) <= window);
        assert!(
            plan.pacer.interval_for(100_000)
                >= window.checked_sub(Duration::from_micros(10)).unwrap()
        );
    }

    #[test]
    fn the_cadence_follows_the_spacing_of_captured_frames() {
        let mut cadence = FrameCadence::default();

        cadence.observe(1_000);
        let first = cadence.smoothed;
        cadence.observe(8_000);
        let second = cadence.smoothed;
        for step in 2..200 {
            cadence.observe(1_000 + step * 8_000);
        }

        assert_eq!(first, None);
        assert_eq!(second, Some(Duration::from_millis(7)));
        assert!(
            cadence.smoothed.unwrap().abs_diff(Duration::from_millis(8))
                < Duration::from_micros(10)
        );
    }

    #[test]
    fn the_window_is_a_share_of_the_faster_of_the_measured_and_configured_interval() {
        let pacing = Pacing {
            frame_interval: Duration::from_millis(16),
            window_percent: 50,
        };

        assert_eq!(pacing.window(None), Duration::from_millis(8));
        assert_eq!(
            pacing.window(Some(Duration::from_millis(4))),
            Duration::from_millis(2)
        );
        assert_eq!(
            pacing.window(Some(Duration::from_millis(33))),
            Duration::from_millis(8)
        );
    }

    #[test]
    fn a_small_frame_leaves_in_one_burst_per_datagram() {
        let plan = BurstPlan::new(3, 3_000, Duration::from_millis(8));

        assert_eq!(plan.datagrams_per_burst, 1);
    }
}
