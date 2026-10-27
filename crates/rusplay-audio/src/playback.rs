use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use pipewire::{
    context::ContextRc,
    keys,
    main_loop::MainLoopRc,
    properties::properties,
    spa::{pod::Pod, utils::Direction},
    stream::{StreamBox, StreamFlags, StreamState},
};
use rtrb::Consumer;
use rusplay_config::AudioConfig;
use rusplay_core::ChannelLayout;

use crate::{
    AudioError, AudioOutput, AudioOutputStats, PcmResampler,
    pw_common::{
        NegotiatedFormat, PipewireThread, StartupSender, format_pod_bytes, negotiated_format,
        node_latency,
    },
    ring::{RingWriter, sample_ring},
};

const BYTES_PER_SAMPLE: usize = size_of::<f32>();
const VIRTUAL_SOURCE_NAME: &str = "rusplay-microphone";
const VIRTUAL_SOURCE_DESCRIPTION: &str = "Rusplay Microphone";
const VIRTUAL_SOURCE_CLASS: &str = "Audio/Source";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackTarget {
    #[default]
    Device,
    VirtualSource,
}

#[derive(Debug, Default)]
struct PlaybackCounters {
    underruns: AtomicU64,
    trims: AtomicU64,
}

struct Playout {
    consumer: Consumer<f32>,
    channels: usize,
    target_frames: usize,
    playing: bool,
    counters: Arc<PlaybackCounters>,
}

impl Playout {
    fn new(
        consumer: Consumer<f32>,
        channels: usize,
        target_frames: usize,
        counters: Arc<PlaybackCounters>,
    ) -> Self {
        Self {
            consumer,
            channels,
            target_frames,
            playing: false,
            counters,
        }
    }

    fn render(&mut self, out: &mut [u8]) -> usize {
        let frame_bytes = self.channels * BYTES_PER_SAMPLE;
        let wanted = out.len() / frame_bytes;
        let mut available = self.consumer.slots() / self.channels;

        let floor = self.target_frames.max(wanted);

        if !self.playing && available >= floor {
            self.playing = true;
        }

        if self.playing && available > floor * 2 {
            let surplus = (available - floor) * self.channels;
            if let Ok(chunk) = self.consumer.read_chunk(surplus) {
                chunk.commit_all();
            }
            self.counters.trims.fetch_add(1, Ordering::Relaxed);
            available = floor;
        }

        let taken = if self.playing {
            available.min(wanted)
        } else {
            0
        };
        if self.playing && taken < wanted {
            self.counters.underruns.fetch_add(1, Ordering::Relaxed);
            self.playing = false;
        }

        let mut written = 0;
        if taken > 0
            && let Ok(chunk) = self.consumer.read_chunk(taken * self.channels)
        {
            let (first, second) = chunk.as_slices();
            for sample in first.iter().chain(second) {
                out[written..written + BYTES_PER_SAMPLE].copy_from_slice(&sample.to_le_bytes());
                written += BYTES_PER_SAMPLE;
            }
            chunk.commit_all();
        }
        out[written..wanted * frame_bytes].fill(0);

        wanted
    }
}

struct PlaybackCallbacks {
    playout: Playout,
    startup: Option<StartupSender>,
    channels: usize,
    fixed_format: Option<NegotiatedFormat>,
}

struct PlaybackSettings {
    layout: ChannelLayout,
    rate: u32,
    latency_frames: usize,
    target: PlaybackTarget,
}

fn run_playback(
    settings: &PlaybackSettings,
    callbacks: PlaybackCallbacks,
    quit: pipewire::channel::Receiver<()>,
) -> Result<(), AudioError> {
    pipewire::init();

    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = match settings.target {
        PlaybackTarget::Device => properties! {
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CATEGORY => "Playback",
            *keys::MEDIA_ROLE => "Game",
            *keys::NODE_NAME => "rusplay-playback",
        },
        PlaybackTarget::VirtualSource => properties! {
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CLASS => VIRTUAL_SOURCE_CLASS,
            *keys::NODE_NAME => VIRTUAL_SOURCE_NAME,
            *keys::NODE_DESCRIPTION => VIRTUAL_SOURCE_DESCRIPTION,
        },
    };
    props.insert(
        *keys::NODE_LATENCY,
        node_latency(settings.latency_frames, settings.rate),
    );

    let stream = StreamBox::new(&core, "rusplay-playback", props)?;

    let _listener = stream
        .add_local_listener_with_user_data(callbacks)
        .param_changed(|_, state, id, param| {
            let Some(format) = negotiated_format(id, param) else {
                return;
            };
            let Some(startup) = state.startup.take() else {
                return;
            };

            let outcome = if format.channels == state.channels {
                Ok(format)
            } else {
                Err(AudioError::PipeWireChannelMismatch {
                    expected: state.channels,
                    found: format.channels,
                })
            };
            let _ = startup.send(outcome);
        })
        .state_changed(|_, state, _, new| match new {
            StreamState::Error(reason) => {
                if let Some(startup) = state.startup.take() {
                    let _ = startup.send(Err(AudioError::PipeWireStream { reason }));
                }
            }
            StreamState::Paused | StreamState::Streaming => {
                if let Some(format) = state.fixed_format
                    && let Some(startup) = state.startup.take()
                {
                    let _ = startup.send(Ok(format));
                }
            }
            _ => {}
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = usize::try_from(buffer.requested()).unwrap_or(usize::MAX);
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let Some(mapped) = data.data() else {
                return;
            };

            let frame_bytes = state.channels * BYTES_PER_SAMPLE;
            let capacity = mapped.len() / frame_bytes;
            let frames = if requested == 0 {
                capacity
            } else {
                capacity.min(requested)
            };

            let rendered = state.playout.render(&mut mapped[..frames * frame_bytes]);

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = i32::try_from(frame_bytes).unwrap_or(i32::MAX);
            *chunk.size_mut() = u32::try_from(rendered * frame_bytes).unwrap_or(u32::MAX);
        })
        .register()?;

    let pod_bytes = format_pod_bytes(settings.layout, settings.rate)?;
    let pod = Pod::from_bytes(&pod_bytes).ok_or(AudioError::PipeWireFormatEncode)?;

    let flags = match settings.target {
        PlaybackTarget::Device => {
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS
        }
        PlaybackTarget::VirtualSource => StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
    };

    stream.connect(Direction::Output, None, flags, &mut [pod])?;

    let loop_handle = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| loop_handle.quit());

    mainloop.run();
    Ok(())
}

fn thread_name(target: PlaybackTarget) -> &'static str {
    match target {
        PlaybackTarget::Device => "rusplay-audio-playback",
        PlaybackTarget::VirtualSource => "rusplay-microphone-source",
    }
}

pub struct AudioPlayback {
    writer: RingWriter,
    resampler: PcmResampler,
    converted: Vec<f32>,
    thread: PipewireThread,
    counters: Arc<PlaybackCounters>,
    overflows: u64,
    native_rate: u32,
}

impl AudioPlayback {
    pub fn start(config: &AudioConfig) -> Result<Self, AudioError> {
        Self::start_to(config, PlaybackTarget::Device)
    }

    pub fn start_to(config: &AudioConfig, target: PlaybackTarget) -> Result<Self, AudioError> {
        let settings = PlaybackSettings {
            layout: config.channels,
            rate: config.sample_rate.hz(),
            latency_frames: config.frame_samples(),
            target,
        };
        let channels = usize::from(config.channels.channel_count());
        let target_frames = settings.rate as usize * config.jitter_buffer_ms as usize / 1000;

        let (writer, consumer) = sample_ring(settings.rate, channels);
        let counters = Arc::new(PlaybackCounters::default());

        let playout = Playout::new(consumer, channels, target_frames, Arc::clone(&counters));
        let fixed_format = match target {
            PlaybackTarget::Device => None,
            PlaybackTarget::VirtualSource => Some(NegotiatedFormat {
                rate: settings.rate,
                channels,
            }),
        };
        let callbacks = PlaybackCallbacks {
            playout,
            startup: None,
            channels,
            fixed_format,
        };

        let (thread, native) = PipewireThread::spawn(thread_name(target), move |quit, startup| {
            let mut callbacks = callbacks;
            callbacks.startup = Some(startup);
            run_playback(&settings, callbacks, quit)
        })?;

        let resampler = PcmResampler::new(
            config.sample_rate.hz(),
            native.rate,
            config.channels.channel_count(),
        )?;

        Ok(Self {
            writer,
            resampler,
            converted: Vec::new(),
            thread,
            counters,
            overflows: 0,
            native_rate: native.rate,
        })
    }

    pub fn native_rate(&self) -> u32 {
        self.native_rate
    }

    pub fn buffered_frames(&self) -> usize {
        self.writer.buffered_frames()
    }

    pub fn underruns(&self) -> u64 {
        self.counters.underruns.load(Ordering::Relaxed)
    }

    pub fn trims(&self) -> u64 {
        self.counters.trims.load(Ordering::Relaxed)
    }

    pub fn overflows(&self) -> u64 {
        self.overflows
    }

    pub fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError> {
        if !self.thread.is_running() {
            return Err(AudioError::PipeWireStopped);
        }

        let dropped = if self.resampler.is_passthrough() {
            self.writer.write(pcm)
        } else {
            self.converted.clear();
            self.resampler.push(pcm, &mut self.converted)?;
            self.writer.write(&self.converted)
        };
        self.overflows += dropped as u64;

        Ok(dropped)
    }
}

impl AudioOutput for AudioPlayback {
    fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError> {
        AudioPlayback::submit(self, pcm)
    }

    fn stats(&self) -> AudioOutputStats {
        AudioOutputStats {
            underruns: self.underruns(),
            trims: self.trims(),
            overflows: self.overflows(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNELS: usize = 2;
    const FRAME_BYTES: usize = CHANNELS * BYTES_PER_SAMPLE;

    struct Fixture {
        writer: RingWriter,
        playout: Playout,
        counters: Arc<PlaybackCounters>,
    }

    fn fixture(target_frames: usize) -> Fixture {
        let (writer, consumer) = sample_ring(48_000, CHANNELS);
        let counters = Arc::new(PlaybackCounters::default());
        let playout = Playout::new(consumer, CHANNELS, target_frames, Arc::clone(&counters));
        Fixture {
            writer,
            playout,
            counters,
        }
    }

    fn ramp(frames: usize, start: usize) -> Vec<f32> {
        (0..frames * CHANNELS)
            .map(|index| f32::from(u16::try_from(start * CHANNELS + index).unwrap()))
            .collect()
    }

    fn samples(bytes: &[u8]) -> Vec<f32> {
        let (chunks, _) = bytes.as_chunks::<BYTES_PER_SAMPLE>();
        chunks
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect()
    }

    fn underruns(fixture: &Fixture) -> u64 {
        fixture.counters.underruns.load(Ordering::Relaxed)
    }

    fn trims(fixture: &Fixture) -> u64 {
        fixture.counters.trims.load(Ordering::Relaxed)
    }

    #[test]
    fn silence_plays_until_the_target_is_buffered() {
        let mut fixture = fixture(480);
        let mut out = vec![9; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(300, 0));
        fixture.playout.render(&mut out);

        assert!(out.iter().all(|byte| *byte == 0));
        assert_eq!(underruns(&fixture), 0);
    }

    #[test]
    fn playback_starts_in_order_once_the_target_is_reached() {
        let mut fixture = fixture(480);
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(600, 0));
        let rendered = fixture.playout.render(&mut out);

        assert_eq!(rendered, 240);
        assert_eq!(samples(&out), ramp(240, 0));

        fixture.playout.render(&mut out);
        assert_eq!(samples(&out), ramp(240, 240));
    }

    #[test]
    fn an_underrun_pads_with_silence_and_rebuffers() {
        let mut fixture = fixture(240);
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(300, 0));
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);

        let tail = samples(&out);
        assert_eq!(&tail[..60 * CHANNELS], &ramp(60, 240)[..]);
        assert!(tail[60 * CHANNELS..].iter().all(|sample| *sample == 0.0));
        assert_eq!(underruns(&fixture), 1);

        fixture.writer.write(&ramp(100, 300));
        fixture.playout.render(&mut out);
        assert!(samples(&out).iter().all(|sample| *sample == 0.0));
        assert_eq!(underruns(&fixture), 1);
    }

    #[test]
    fn a_buffer_that_grows_past_the_ceiling_is_trimmed_to_the_target() {
        let mut fixture = fixture(240);
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(240, 0));
        fixture.playout.render(&mut out);

        fixture.writer.write(&ramp(2000, 240));
        fixture.playout.render(&mut out);

        assert_eq!(trims(&fixture), 1);
        assert_eq!(fixture.writer.buffered_frames(), 0);
    }

    #[test]
    fn a_zero_target_still_waits_for_one_full_quantum() {
        let mut fixture = fixture(0);
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(100, 0));
        fixture.playout.render(&mut out);
        assert!(samples(&out).iter().all(|sample| *sample == 0.0));

        fixture.writer.write(&ramp(200, 100));
        fixture.playout.render(&mut out);
        assert_eq!(samples(&out), ramp(240, 0));
    }
}

#[cfg(test)]
mod live_tests {
    use std::{f32::consts::TAU, time::Duration};

    use super::*;
    use crate::AudioCapture;

    #[test]
    #[ignore = "needs a running PipeWire session and PIPEWIRE_NODE set to a null sink"]
    fn playback_reaches_a_capture_of_the_same_sink() {
        let target = std::env::var("PIPEWIRE_NODE");
        assert!(
            target.is_ok(),
            "set PIPEWIRE_NODE to a null sink so the test stays silent"
        );

        let config = AudioConfig::default();
        let channels = usize::from(config.channels.channel_count());
        let frame_samples = config.opus.frame_duration.samples_at(config.sample_rate);

        let mut capture = AudioCapture::start(&config).unwrap();
        let mut playback = AudioPlayback::start(&config).unwrap();

        let mut sent = 0;
        let mut peak = 0.0f32;
        let mut heard = vec![0.0; frame_samples * channels];
        for _ in 0..400 {
            let mut frame = vec![0.0; frame_samples * channels];
            for index in 0..frame_samples {
                let phase =
                    TAU * 997.0 * f32::from(u16::try_from(sent % 48_000).unwrap()) / 48_000.0;
                frame[index * channels] = 0.5 * phase.sin();
                frame[index * channels + 1] = 0.5 * phase.sin();
                sent += 1;
            }
            playback.submit(&frame).unwrap();

            if capture
                .read_frame(&mut heard, Duration::from_millis(100))
                .unwrap()
            {
                peak = heard
                    .iter()
                    .fold(peak, |peak, sample| peak.max(sample.abs()));
            }
        }

        assert!(peak > 0.4, "captured peak {peak}");
        assert_eq!(playback.native_rate(), config.sample_rate.hz());
    }

    #[test]
    #[ignore = "needs a running PipeWire session and PIPEWIRE_NODE=rusplay-microphone"]
    fn a_virtual_source_is_heard_by_a_source_capture() {
        use crate::{AudioCapture, CaptureTarget};

        assert_eq!(
            std::env::var("PIPEWIRE_NODE").as_deref(),
            Ok(VIRTUAL_SOURCE_NAME),
            "set PIPEWIRE_NODE=rusplay-microphone so the capture follows the virtual source"
        );

        let config = AudioConfig::default().microphone_stream();
        let frame_samples = config.frame_samples();

        let mut playback = AudioPlayback::start_to(&config, PlaybackTarget::VirtualSource).unwrap();
        let mut capture = AudioCapture::start_from(&config, CaptureTarget::Source).unwrap();

        let mut sent = 0;
        let mut peak = 0.0f32;
        let mut heard = vec![0.0; frame_samples];
        for _ in 0..400 {
            let frame: Vec<f32> = (0..frame_samples)
                .map(|_| {
                    let phase =
                        TAU * 997.0 * f32::from(u16::try_from(sent % 48_000).unwrap()) / 48_000.0;
                    sent += 1;
                    0.5 * phase.sin()
                })
                .collect();
            playback.submit(&frame).unwrap();

            if capture
                .read_frame(&mut heard, Duration::from_millis(100))
                .unwrap()
            {
                peak = heard
                    .iter()
                    .fold(peak, |peak, sample| peak.max(sample.abs()));
            }
        }

        assert!(peak > 0.4, "captured peak {peak}");
    }
}
