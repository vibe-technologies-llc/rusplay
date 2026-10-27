use std::{
    sync::{
        Arc,
        mpsc::{Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use rusplay_audio::{AudioOutput, ConcealingPlayer, OpusAudioDecoder, PlayerStats};
use rusplay_config::{AudioConfig, NetworkConfig};
use rusplay_net::{Reassembler, ReassemblyEvent, ReassemblyLimits};
use rusplay_protocol::StreamKind;

use crate::{HostError, shared::Shared, stats::Counters};

const IDLE_WAKE: Duration = Duration::from_millis(20);

pub type MicrophoneOutputFactory =
    Arc<dyn Fn(&AudioConfig) -> Result<Box<dyn AudioOutput>, HostError> + Send + Sync>;

#[cfg(feature = "pipewire")]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn default_microphone_output() -> Option<MicrophoneOutputFactory> {
    Some(Arc::new(|config| {
        Ok(Box::new(rusplay_audio::AudioPlayback::start_to(
            config,
            rusplay_audio::PlaybackTarget::VirtualSource,
        )?) as Box<dyn AudioOutput>)
    }))
}

#[cfg(not(feature = "pipewire"))]
pub(crate) fn default_microphone_output() -> Option<MicrophoneOutputFactory> {
    None
}

pub(crate) struct MicrophoneLinks {
    pub shared: Arc<Shared>,
    pub datagrams: Receiver<Vec<u8>>,
}

pub(crate) fn run(
    config: AudioConfig,
    network: &NetworkConfig,
    factory: &MicrophoneOutputFactory,
    links: &MicrophoneLinks,
) -> Result<(), HostError> {
    let result = serve(&config.microphone_stream(), network, factory, links);
    links.shared.stop();

    result
}

fn serve(
    stream: &AudioConfig,
    network: &NetworkConfig,
    factory: &MicrophoneOutputFactory,
    links: &MicrophoneLinks,
) -> Result<(), HostError> {
    let decoder = Box::new(OpusAudioDecoder::new(stream)?);
    let output = factory(stream)?;
    let mut player = ConcealingPlayer::new(decoder, output, stream);
    let mut reassembler = Reassembler::new(StreamKind::Microphone, ReassemblyLimits::from(network));
    let mut events = Vec::new();
    let counters = &links.shared.counters;

    while !links.shared.stopped() {
        let wait = reassembler.next_timeout().map_or(IDLE_WAKE, |due| {
            due.saturating_duration_since(Instant::now()).min(IDLE_WAKE)
        });

        match links.datagrams.recv_timeout(wait) {
            Ok(datagram) => {
                push(&mut reassembler, &datagram, &mut events, counters);
                while let Ok(datagram) = links.datagrams.try_recv() {
                    push(&mut reassembler, &datagram, &mut events, counters);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }

        reassembler.poll_timeouts(Instant::now(), &mut events);
        let outcome = play(&mut player, &mut events, counters);
        publish(&player, counters);
        outcome?;
    }

    Ok(())
}

fn push(
    reassembler: &mut Reassembler,
    datagram: &[u8],
    events: &mut Vec<ReassemblyEvent>,
    counters: &Counters,
) {
    if reassembler.push(datagram, Instant::now(), events).is_err() {
        counters.malformed_datagrams.increment();
    }
}

fn play(
    player: &mut ConcealingPlayer,
    events: &mut Vec<ReassemblyEvent>,
    counters: &Counters,
) -> Result<(), HostError> {
    for event in events.drain(..) {
        if let ReassemblyEvent::Frame(frame) = event {
            counters.microphone_packets_received.increment();
            player.process(frame.frame, &frame.data)?;
        }
    }

    Ok(())
}

fn publish(player: &ConcealingPlayer, counters: &Counters) {
    let PlayerStats {
        late,
        lost,
        concealed,
        decode_errors,
        ..
    } = player.stats();
    let output = player.output_stats();

    counters.microphone_packets_late.set(late);
    counters.microphone_packets_lost.set(lost);
    counters.microphone_frames_concealed.set(concealed);
    counters.microphone_decode_errors.set(decode_errors);
    counters.microphone_underruns.set(output.underruns);
    counters.microphone_overflows.set(output.overflows);
}
