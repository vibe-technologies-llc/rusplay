use std::{
    f32::consts::PI,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
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
    ring::{RingWriter, playback_ring_rate, ring_milliseconds, sample_ring},
};

const BYTES_PER_SAMPLE: usize = size_of::<f32>();
const MICROS_PER_SECOND: u64 = 1_000_000;
const PARTS_PER_MILLION: f64 = 1e-6;
const VIRTUAL_SOURCE_NAME: &str = "rusplay-microphone";
const VIRTUAL_SOURCE_DESCRIPTION: &str = "Rusplay Microphone";
const VIRTUAL_SOURCE_CLASS: &str = "Audio/Source";
const DECLICK_SPAN: Duration = Duration::from_millis(2);
const LEVEL_SMOOTHING_SECONDS: f64 = 1.0;
const LOOP_RADIANS_PER_SECOND: f64 = 0.1;
const PROPORTIONAL_GAIN: f64 = 2.0 * LOOP_RADIANS_PER_SECOND;
const INTEGRAL_GAIN: f64 = LOOP_RADIANS_PER_SECOND * LOOP_RADIANS_PER_SECOND;
const NOMINAL_SPEED: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackTarget {
    #[default]
    Device,
    VirtualSource,
}

#[derive(Debug)]
struct PlaybackCounters {
    underruns: AtomicU64,
    trims: AtomicU64,
    jitter_target_us: AtomicU32,
    speed_bits: AtomicU64,
}

impl Default for PlaybackCounters {
    fn default() -> Self {
        Self {
            underruns: AtomicU64::new(0),
            trims: AtomicU64::new(0),
            jitter_target_us: AtomicU32::new(0),
            speed_bits: AtomicU64::new(NOMINAL_SPEED.to_bits()),
        }
    }
}

impl PlaybackCounters {
    fn speed(&self) -> f64 {
        f64::from_bits(self.speed_bits.load(Ordering::Relaxed))
    }

    fn set_speed(&self, speed: f64) {
        self.speed_bits.store(speed.to_bits(), Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct JitterPolicy {
    floor: Duration,
    ceiling: Duration,
    step: Duration,
    relax: Duration,
    declick: Duration,
    widest_correction: f64,
}

impl JitterPolicy {
    fn from_config(config: &AudioConfig) -> Self {
        let frame_micros = config.frame_samples() as u64 * MICROS_PER_SECOND
            / u64::from(config.sample_rate.hz().max(1));
        let millis = |value: u32| Duration::from_millis(u64::from(value));

        Self {
            floor: millis(config.jitter_buffer_ms),
            ceiling: millis(config.jitter_buffer_max_ms.max(config.jitter_buffer_ms)),
            step: Duration::from_micros(frame_micros.max(1)),
            relax: millis(config.jitter_relax_ms),
            declick: DECLICK_SPAN,
            widest_correction: f64::from(config.rate_match_max_ppm) * PARTS_PER_MILLION,
        }
    }

    fn follows_clock(&self) -> bool {
        self.widest_correction > 0.0
    }

    fn widest_speed(&self) -> f64 {
        NOMINAL_SPEED + self.widest_correction
    }
}

fn frames_for(rate: u32, span: Duration) -> usize {
    let frames = u128::from(rate) * span.as_micros() / u128::from(MICROS_PER_SECOND);

    usize::try_from(frames).unwrap_or(usize::MAX)
}

fn whole_micros(span: Duration) -> u32 {
    u32::try_from(span.as_micros()).unwrap_or(u32::MAX)
}

fn frame_count(frames: usize) -> f64 {
    f64::from(u32::try_from(frames).unwrap_or(u32::MAX))
}

fn blend_weight(frame: usize, span: usize) -> f32 {
    let frame = f32::from(u16::try_from(frame + 1).unwrap_or(u16::MAX));
    let span = f32::from(u16::try_from(span).unwrap_or(u16::MAX));

    (frame / span).min(1.0)
}

fn fade_gain(position: usize, span: usize) -> f32 {
    if position >= span {
        return 1.0;
    }

    let phase = blend_weight(position, span + 1) * PI;

    0.5 - 0.5 * phase.cos()
}

#[derive(Debug, Clone, Copy)]
struct ClockRecovery {
    widest_correction: f64,
    level: Option<f64>,
    drift: f64,
}

impl ClockRecovery {
    fn new(widest_correction: f64) -> Self {
        Self {
            widest_correction,
            level: None,
            drift: 0.0,
        }
    }

    fn restart(&mut self) {
        self.level = None;
    }

    fn smoothed(&mut self, level: f64, seconds: f64) -> f64 {
        let previous = self.level.unwrap_or(level);
        let smoothed =
            previous + (level - previous) * seconds / (LEVEL_SMOOTHING_SECONDS + seconds);
        self.level = Some(smoothed);

        smoothed
    }

    fn follow(&mut self, level: usize, target: usize, quantum: usize, rate: u32) -> Following {
        let level = frame_count(level);
        let rate = f64::from(rate.max(1));
        let seconds = frame_count(quantum) / rate;
        let widest = self.widest_correction;

        let smoothed = self.smoothed(level, seconds);
        let surplus_seconds = (smoothed - frame_count(target)) / rate;
        self.drift =
            (self.drift + INTEGRAL_GAIN * surplus_seconds * seconds).clamp(-widest, widest);
        let correction = (PROPORTIONAL_GAIN * surplus_seconds + self.drift).clamp(-widest, widest);

        Following {
            speed: NOMINAL_SPEED + correction,
            dip: (smoothed - level).max(0.0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Following {
    speed: f64,
    dip: f64,
}

struct Playout {
    consumer: Consumer<f32>,
    channels: usize,
    policy: JitterPolicy,
    target: Duration,
    negotiated_rate: AtomicU32,
    calm_frames: usize,
    deepest_dip: f64,
    playing: bool,
    faded_in: usize,
    decayed: usize,
    last_frame: Vec<f32>,
    recovery: ClockRecovery,
    counters: Arc<PlaybackCounters>,
}

impl Playout {
    fn new(
        consumer: Consumer<f32>,
        channels: usize,
        policy: JitterPolicy,
        rate: u32,
        counters: Arc<PlaybackCounters>,
    ) -> Self {
        counters
            .jitter_target_us
            .store(whole_micros(policy.floor), Ordering::Relaxed);

        Self {
            consumer,
            channels,
            policy,
            target: policy.floor,
            negotiated_rate: AtomicU32::new(rate),
            calm_frames: 0,
            deepest_dip: 0.0,
            playing: false,
            faded_in: 0,
            decayed: usize::MAX,
            last_frame: vec![0.0; channels],
            recovery: ClockRecovery::new(policy.widest_correction),
            counters,
        }
    }

    fn retarget(&self, rate: u32) {
        self.negotiated_rate.store(rate, Ordering::Relaxed);
    }

    fn rate(&self) -> u32 {
        self.negotiated_rate.load(Ordering::Relaxed)
    }

    fn target_frames(&self) -> usize {
        frames_for(self.rate(), self.target)
    }

    fn floor_frames(&self, wanted: usize) -> usize {
        self.target_frames() + wanted
    }

    fn step_frames(&self) -> usize {
        frames_for(self.rate(), self.policy.step)
    }

    fn declick_frames(&self) -> usize {
        frames_for(self.rate(), self.policy.declick)
    }

    fn restart_calm_window(&mut self) {
        self.calm_frames = 0;
        self.deepest_dip = 0.0;
    }

    fn set_target(&mut self, target: Duration) {
        self.target = target;
        self.restart_calm_window();
        self.counters
            .jitter_target_us
            .store(whole_micros(target), Ordering::Relaxed);
    }

    fn raise_target(&mut self) {
        let raised = (self.target + self.policy.step).min(self.policy.ceiling);
        self.set_target(raised);
    }

    fn relaxed_target(&self) -> Duration {
        self.target
            .saturating_sub(self.policy.step)
            .max(self.policy.floor)
    }

    fn relax_due(&self) -> bool {
        !self.policy.relax.is_zero()
            && self.target > self.policy.floor
            && self.calm_frames >= frames_for(self.rate(), self.policy.relax)
    }

    fn relax_keeps_headroom(&self) -> bool {
        let relaxed = frame_count(frames_for(self.rate(), self.relaxed_target()));
        let headroom = frame_count(self.step_frames()) / 2.0;

        relaxed - self.deepest_dip >= headroom
    }

    fn surplus_to_discard(&mut self, available: usize, wanted: usize) -> usize {
        let floor = self.floor_frames(wanted);
        if available > floor * 2 {
            self.counters.trims.fetch_add(1, Ordering::Relaxed);
            self.recovery.restart();
            return available - floor;
        }

        if !self.relax_due() {
            return 0;
        }
        if !self.relax_keeps_headroom() {
            self.restart_calm_window();
            return 0;
        }

        self.set_target(self.relaxed_target());
        if self.policy.follows_clock() {
            return 0;
        }

        available
            .saturating_sub(self.floor_frames(wanted))
            .min(self.step_frames())
    }

    fn follow_clock(&mut self, level: usize, quantum: usize) {
        let following = self
            .recovery
            .follow(level, self.target_frames(), quantum, self.rate());

        self.deepest_dip = self.deepest_dip.max(following.dip);
        self.counters.set_speed(following.speed);
    }

    fn render(&mut self, out: &mut [u8]) -> usize {
        let frame_bytes = self.channels * BYTES_PER_SAMPLE;
        let wanted = out.len() / frame_bytes;
        let available = self.consumer.slots() / self.channels;
        let was_playing = self.playing;

        if !self.playing && available >= self.floor_frames(wanted) {
            self.playing = true;
            self.faded_in = 0;
            self.recovery.restart();
        }

        let discarded = if self.playing {
            self.surplus_to_discard(available, wanted)
        } else {
            0
        };

        let taken = if self.playing {
            (available - discarded).min(wanted)
        } else {
            0
        };
        if self.playing && taken < wanted {
            self.counters.underruns.fetch_add(1, Ordering::Relaxed);
            self.playing = false;
            self.decayed = 0;
            self.raise_target();
        }
        if self.playing {
            self.calm_frames = self.calm_frames.saturating_add(wanted);
            self.follow_clock(available - discarded - taken, wanted);
        }

        let crossfade = was_playing && discarded > 0;
        let played = self.copy_out(out, discarded, taken, crossfade);
        let decayed = self.decay_into(&mut out[played..wanted * frame_bytes]);
        out[played + decayed..wanted * frame_bytes].fill(0);

        wanted
    }

    fn copy_out(
        &mut self,
        out: &mut [u8],
        discarded: usize,
        taken: usize,
        crossfade: bool,
    ) -> usize {
        let channels = self.channels;
        if taken == 0 && discarded == 0 {
            return 0;
        }
        let Ok(chunk) = self.consumer.read_chunk((discarded + taken) * channels) else {
            return 0;
        };

        let (first, second) = chunk.as_slices();
        let sample = |index: usize| {
            first
                .get(index)
                .copied()
                .unwrap_or_else(|| second[index - first.len()])
        };
        let fade_span = frames_for(
            self.negotiated_rate.load(Ordering::Relaxed),
            self.policy.declick,
        );

        let mut written = 0;
        for frame in 0..taken {
            let weight = if crossfade {
                blend_weight(frame, taken)
            } else {
                1.0
            };
            let gain = fade_gain(self.faded_in.saturating_add(frame), fade_span);
            for channel in 0..channels {
                let fresh = sample((discarded + frame) * channels + channel);
                let blended = if crossfade {
                    let stale = sample(frame * channels + channel);
                    stale * (1.0 - weight) + fresh * weight
                } else {
                    fresh
                };
                let value = blended * gain;
                self.last_frame[channel] = value;
                out[written..written + BYTES_PER_SAMPLE].copy_from_slice(&value.to_le_bytes());
                written += BYTES_PER_SAMPLE;
            }
        }
        chunk.commit_all();
        self.faded_in = self.faded_in.saturating_add(taken);

        written
    }

    fn decay_into(&mut self, out: &mut [u8]) -> usize {
        let span = self.declick_frames();
        if self.playing || self.decayed >= span {
            return 0;
        }

        let frame_bytes = self.channels * BYTES_PER_SAMPLE;
        let frames = (out.len() / frame_bytes).min(span - self.decayed);

        let mut written = 0;
        for frame in 0..frames {
            let gain = 1.0 - fade_gain(self.decayed + frame, span);
            for held in &self.last_frame {
                let value = held * gain;
                out[written..written + BYTES_PER_SAMPLE].copy_from_slice(&value.to_le_bytes());
                written += BYTES_PER_SAMPLE;
            }
        }
        self.decayed += frames;

        written
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
            state.playout.retarget(format.rate);

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

struct Feed {
    writer: RingWriter,
    resampler: PcmResampler,
    converted: Vec<f32>,
    counters: Arc<PlaybackCounters>,
}

impl Feed {
    fn new(
        writer: RingWriter,
        config: &AudioConfig,
        policy: &JitterPolicy,
        native_rate: u32,
        counters: Arc<PlaybackCounters>,
    ) -> Result<Self, AudioError> {
        let configured_rate = config.sample_rate.hz();
        let channels = config.channels.channel_count();

        let resampler = if policy.follows_clock() {
            PcmResampler::rate_matching(
                configured_rate,
                native_rate,
                channels,
                policy.widest_speed(),
            )?
        } else {
            PcmResampler::new(configured_rate, native_rate, channels)?
        };

        Ok(Self {
            writer,
            resampler,
            converted: Vec::new(),
            counters,
        })
    }

    fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError> {
        if self.resampler.is_passthrough() {
            return Ok(self.writer.write(pcm));
        }

        self.converted.clear();
        self.resampler.set_speed(self.counters.speed())?;
        self.resampler.push(pcm, &mut self.converted)?;

        Ok(self.writer.write(&self.converted))
    }
}

pub struct AudioPlayback {
    feed: Feed,
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
        let policy = JitterPolicy::from_config(config);

        let (writer, consumer) = sample_ring(
            playback_ring_rate(settings.rate),
            channels,
            ring_milliseconds(config.jitter_buffer_max_ms.max(config.jitter_buffer_ms)),
        );
        let counters = Arc::new(PlaybackCounters::default());

        let playout = Playout::new(
            consumer,
            channels,
            policy,
            settings.rate,
            Arc::clone(&counters),
        );
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

        let feed = Feed::new(writer, config, &policy, native.rate, Arc::clone(&counters))?;

        Ok(Self {
            feed,
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
        self.feed.writer.buffered_frames()
    }

    pub fn underruns(&self) -> u64 {
        self.counters.underruns.load(Ordering::Relaxed)
    }

    pub fn trims(&self) -> u64 {
        self.counters.trims.load(Ordering::Relaxed)
    }

    pub fn jitter_target_us(&self) -> u32 {
        self.counters.jitter_target_us.load(Ordering::Relaxed)
    }

    pub fn overflows(&self) -> u64 {
        self.overflows
    }

    pub fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError> {
        if !self.thread.is_running() {
            return Err(AudioError::PipeWireStopped);
        }

        let dropped = self.feed.submit(pcm)?;
        self.overflows += self.feed.writer.dropped_frames(dropped) as u64;

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
            jitter_target_us: self.jitter_target_us(),
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

    fn fixed(jitter_ms: u64) -> JitterPolicy {
        adaptive(jitter_ms, jitter_ms, 0)
    }

    fn adaptive(floor_ms: u64, ceiling_ms: u64, relax_ms: u64) -> JitterPolicy {
        JitterPolicy {
            floor: Duration::from_millis(floor_ms),
            ceiling: Duration::from_millis(ceiling_ms),
            step: Duration::from_millis(5),
            relax: Duration::from_millis(relax_ms),
            declick: Duration::ZERO,
            widest_correction: 0.0,
        }
    }

    fn fixture(jitter_ms: u64) -> Fixture {
        fixture_with(fixed(jitter_ms))
    }

    fn fixture_with(policy: JitterPolicy) -> Fixture {
        let (writer, consumer) = sample_ring(
            playback_ring_rate(48_000),
            CHANNELS,
            ring_milliseconds(u32::try_from(policy.ceiling.as_millis()).unwrap()),
        );
        let counters = Arc::new(PlaybackCounters::default());
        let playout = Playout::new(consumer, CHANNELS, policy, 48_000, Arc::clone(&counters));
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

    fn jitter_target_us(fixture: &Fixture) -> u32 {
        fixture.counters.jitter_target_us.load(Ordering::Relaxed)
    }

    fn silent(out: &[u8]) -> bool {
        samples(out).iter().all(|sample| *sample == 0.0)
    }

    #[test]
    fn silence_plays_until_the_target_is_buffered() {
        let mut fixture = fixture(10);
        let mut out = vec![9; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(700, 0));
        fixture.playout.render(&mut out);

        assert!(out.iter().all(|byte| *byte == 0));
        assert_eq!(underruns(&fixture), 0);
    }

    #[test]
    fn playback_starts_in_order_once_the_target_is_reached() {
        let mut fixture = fixture(10);
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(800, 0));
        let rendered = fixture.playout.render(&mut out);

        assert_eq!(rendered, 240);
        assert_eq!(samples(&out), ramp(240, 0));

        fixture.playout.render(&mut out);
        assert_eq!(samples(&out), ramp(240, 240));
    }

    #[test]
    fn the_jitter_target_stays_queued_beyond_the_quantum() {
        let mut fixture = fixture(5);
        let mut out = vec![0; 1024 * FRAME_BYTES];

        fixture.writer.write(&ramp(1200, 0));
        fixture.playout.render(&mut out);
        assert!(silent(&out));

        fixture.writer.write(&ramp(64, 1200));
        fixture.playout.render(&mut out);

        assert_eq!(samples(&out), ramp(1024, 0));
        assert_eq!(fixture.writer.buffered_frames(), 240);
    }

    #[test]
    fn an_underrun_pads_with_silence_and_rebuffers() {
        let mut fixture = fixture(5);
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(500, 0));
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);

        let tail = samples(&out);
        assert_eq!(&tail[..20 * CHANNELS], &ramp(20, 480)[..]);
        assert!(tail[20 * CHANNELS..].iter().all(|sample| *sample == 0.0));
        assert_eq!(underruns(&fixture), 1);

        fixture.writer.write(&ramp(100, 500));
        fixture.playout.render(&mut out);
        assert!(silent(&out));

        fixture.writer.write(&ramp(100, 600));
        fixture.playout.render(&mut out);
        assert!(silent(&out));
        assert_eq!(underruns(&fixture), 1);

        fixture.writer.write(&ramp(300, 700));
        fixture.playout.render(&mut out);
        assert_eq!(samples(&out), ramp(240, 500));
        fixture.playout.render(&mut out);
        assert_eq!(samples(&out), ramp(240, 740));
        assert_eq!(underruns(&fixture), 1);
    }

    #[test]
    fn an_underrun_raises_the_target_by_one_codec_frame_up_to_the_ceiling() {
        let mut fixture = fixture_with(adaptive(5, 15, 0));
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(480, 0));
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        assert_eq!(underruns(&fixture), 1);
        assert_eq!(jitter_target_us(&fixture), 10_000);

        fixture.writer.write(&ramp(700, 480));
        fixture.playout.render(&mut out);
        assert!(silent(&out));

        fixture.writer.write(&ramp(20, 1180));
        fixture.playout.render(&mut out);
        assert_eq!(samples(&out), ramp(240, 480));

        for _ in 0..4 {
            fixture.playout.render(&mut out);
        }
        assert_eq!(underruns(&fixture), 2);
        assert_eq!(jitter_target_us(&fixture), 15_000);

        fixture.writer.write(&ramp(1500, 0));
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        assert_eq!(underruns(&fixture), 3);
        assert_eq!(jitter_target_us(&fixture), 15_000);
    }

    #[test]
    fn a_calm_stretch_relaxes_the_target_and_crossfades_out_one_step() {
        let mut fixture = fixture_with(adaptive(5, 15, 20));
        let mut out = vec![0; 240 * FRAME_BYTES];
        fixture.writer.write(&ramp(480, 0));
        for _ in 0..3 {
            fixture.playout.render(&mut out);
        }
        assert_eq!(jitter_target_us(&fixture), 10_000);

        fixture.writer.write(&ramp(1200, 0));
        for block in 0..4 {
            fixture.playout.render(&mut out);
            fixture.writer.write(&ramp(240, 1200 + block * 240));
        }
        assert_eq!(fixture.writer.buffered_frames(), 1200);
        assert_eq!(jitter_target_us(&fixture), 10_000);

        fixture.playout.render(&mut out);

        let blended = samples(&out);
        assert_eq!(jitter_target_us(&fixture), 5_000);
        assert_eq!(fixture.writer.buffered_frames(), 720);
        assert_eq!(trims(&fixture), 0);
        assert!(blended[0] > ramp(1, 960)[0] && blended[0] < ramp(1, 1200)[0]);
        assert_eq!(&blended[239 * CHANNELS..], &ramp(1, 1439)[..]);
        assert_eq!(underruns(&fixture), 1);
    }

    #[test]
    fn a_target_at_its_floor_never_relaxes() {
        let mut fixture = fixture_with(adaptive(5, 15, 5));
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(720, 0));
        for block in 0..4 {
            fixture.playout.render(&mut out);
            fixture.writer.write(&ramp(240, 720 + block * 240));
        }

        assert_eq!(samples(&out), ramp(240, 720));
        assert_eq!(fixture.writer.buffered_frames(), 720);
        assert_eq!(jitter_target_us(&fixture), 5_000);
    }

    #[test]
    fn a_buffer_that_grows_past_the_ceiling_is_trimmed_to_the_target() {
        let mut fixture = fixture(5);
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(240, 0));
        fixture.playout.render(&mut out);

        fixture.writer.write(&ramp(2000, 240));
        fixture.playout.render(&mut out);

        assert_eq!(trims(&fixture), 1);
        assert_eq!(samples(&out), ramp(240, 1760));
        assert_eq!(fixture.writer.buffered_frames(), 240);
    }

    #[test]
    fn the_largest_jitter_buffer_fits_the_ring_and_plays() {
        let mut fixture = fixture(500);
        let mut out = vec![0; 240 * FRAME_BYTES];

        let dropped = fixture.writer.write(&ramp(24_240, 0));
        fixture.playout.render(&mut out);

        assert_eq!(dropped, 0);
        assert_eq!(samples(&out), ramp(240, 0));
    }

    #[test]
    fn the_largest_jitter_buffer_still_trims_drift() {
        let mut fixture = fixture(500);
        let mut out = vec![0; 240 * FRAME_BYTES];
        fixture.writer.write(&ramp(24_240, 0));
        fixture.playout.render(&mut out);

        let dropped = fixture.writer.write(&vec![0.5; 25_000 * CHANNELS]);
        fixture.playout.render(&mut out);

        assert_eq!(dropped, 0);
        assert_eq!(trims(&fixture), 1);
        assert_eq!(fixture.writer.buffered_frames(), 24_000);
    }

    #[test]
    fn the_target_follows_the_negotiated_rate() {
        let mut fixture = fixture(100);
        let mut out = vec![0; 240 * FRAME_BYTES];
        fixture.playout.retarget(96_000);

        fixture.writer.write(&ramp(6_000, 0));
        fixture.playout.render(&mut out);
        assert!(silent(&out));

        fixture.writer.write(&ramp(4_000, 6_000));
        fixture.playout.render(&mut out);
        assert_eq!(samples(&out), ramp(240, 0));
    }

    #[test]
    fn a_zero_target_still_waits_for_one_full_quantum() {
        let mut fixture = fixture(0);
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&ramp(100, 0));
        fixture.playout.render(&mut out);
        assert!(silent(&out));

        fixture.writer.write(&ramp(200, 100));
        fixture.playout.render(&mut out);
        assert_eq!(samples(&out), ramp(240, 0));
    }

    const QUANTUM: usize = 128;
    const PACKET_FRAMES: usize = 240;

    fn clocked(floor_ms: u64, ceiling_ms: u64, relax_ms: u64) -> JitterPolicy {
        JitterPolicy {
            widest_correction: 0.005,
            ..adaptive(floor_ms, ceiling_ms, relax_ms)
        }
    }

    fn declicked(jitter_ms: u64) -> JitterPolicy {
        JitterPolicy {
            declick: Duration::from_millis(2),
            ..fixed(jitter_ms)
        }
    }

    fn steady(frames: usize, level: f32) -> Vec<f32> {
        vec![level; frames * CHANNELS]
    }

    fn left(out: &[u8]) -> Vec<f32> {
        samples(out).into_iter().step_by(CHANNELS).collect()
    }

    struct Link {
        fixture: Fixture,
        host_speed: f64,
        owed_frames: f64,
        packets_sent: u32,
        last_arrival: f64,
        callbacks: u32,
    }

    impl Link {
        fn new(policy: JitterPolicy, drift_ppm: f64) -> Self {
            Self {
                fixture: fixture_with(policy),
                host_speed: 1.0 + drift_ppm * PARTS_PER_MILLION,
                owed_frames: 0.0,
                packets_sent: 0,
                last_arrival: 0.0,
                callbacks: 0,
            }
        }

        fn deliver(&mut self) {
            let speed = self.fixture.counters.speed();
            self.owed_frames += frame_count(PACKET_FRAMES) / speed;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let whole = self.owed_frames as usize;
            self.owed_frames -= frame_count(whole);

            self.fixture.writer.write(&steady(whole, 0.25));
            self.packets_sent += 1;
        }

        fn run(&mut self, seconds: u32, lateness: impl Fn(u32) -> f64) -> Vec<usize> {
            let mut out = vec![0; QUANTUM * FRAME_BYTES];
            let mut levels = Vec::new();
            let end = self.callbacks + seconds * 48_000 / 128;

            while self.callbacks < end {
                let now = f64::from(self.callbacks) * frame_count(QUANTUM);
                loop {
                    let nominal =
                        f64::from(self.packets_sent) * frame_count(PACKET_FRAMES) / self.host_speed;
                    let arrival = (nominal + lateness(self.packets_sent)).max(self.last_arrival);
                    if arrival > now {
                        break;
                    }
                    self.last_arrival = arrival;
                    self.deliver();
                }

                self.fixture.playout.render(&mut out);
                levels.push(self.fixture.writer.buffered_frames());
                self.callbacks += 1;
            }

            levels
        }
    }

    fn mean(levels: &[usize]) -> f64 {
        levels.iter().copied().map(frame_count).sum::<f64>() / frame_count(levels.len())
    }

    fn on_time(_packet: u32) -> f64 {
        0.0
    }

    #[test]
    fn playback_speed_locks_to_a_host_clock_that_runs_fast_or_slow() {
        for drift_ppm in [300.0, -300.0, 40.0] {
            let mut link = Link::new(clocked(10, 50, 10_000), drift_ppm);

            link.run(540, on_time);
            let settled = link.run(60, on_time);

            let speed_error_ppm =
                (link.fixture.counters.speed() - link.host_speed) / PARTS_PER_MILLION;
            assert_eq!(underruns(&link.fixture), 0, "drift {drift_ppm}");
            assert_eq!(trims(&link.fixture), 0, "drift {drift_ppm}");
            assert!(
                speed_error_ppm.abs() < 30.0,
                "drift {drift_ppm}: speed off by {speed_error_ppm} ppm"
            );
            assert!(
                (mean(&settled) - 480.0).abs() < 48.0,
                "drift {drift_ppm}: mean level {}",
                mean(&settled)
            );
        }
    }

    #[test]
    fn without_rate_matching_the_same_drift_ends_in_trims_or_underruns() {
        let mut fast = Link::new(adaptive(10, 50, 10_000), 300.0);
        let mut slow = Link::new(adaptive(10, 50, 10_000), -300.0);

        fast.run(600, on_time);
        slow.run(600, on_time);

        assert!(trims(&fast.fixture) > 0);
        assert!(underruns(&slow.fixture) > 0);
    }

    #[test]
    fn a_calm_stretch_relaxes_by_playing_faster_and_drops_nothing() {
        let mut link = Link::new(clocked(5, 15, 2_000), 0.0);
        let stall = |packet: u32| if packet == 400 { 400.0 } else { 0.0 };

        link.run(4, stall);
        let raised = jitter_target_us(&link.fixture);
        link.run(120, stall);
        let settled = link.run(20, stall);

        assert_eq!(raised, 10_000);
        assert_eq!(jitter_target_us(&link.fixture), 5_000);
        assert_eq!(underruns(&link.fixture), 1);
        assert_eq!(trims(&link.fixture), 0);
        assert!(
            (mean(&settled) - 240.0).abs() < 48.0,
            "mean level {}",
            mean(&settled)
        );
    }

    #[test]
    fn a_target_the_jitter_still_needs_is_not_relaxed() {
        let mut link = Link::new(clocked(5, 50, 2_000), 0.0);
        let hiccups = |packet: u32| if packet % 200 == 199 { 400.0 } else { 0.0 };

        link.run(60, hiccups);
        let learned_underruns = underruns(&link.fixture);
        let learned_target = jitter_target_us(&link.fixture);
        link.run(240, hiccups);

        assert!(learned_target > 5_000);
        assert_eq!(underruns(&link.fixture), learned_underruns);
        assert_eq!(jitter_target_us(&link.fixture), learned_target);
    }

    #[test]
    fn an_underrun_decays_from_the_last_sample_instead_of_cutting() {
        let mut fixture = fixture_with(declicked(5));
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&steady(500, 0.5));
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);
        fixture.playout.render(&mut out);

        let tail = left(&out);
        let largest_step = tail
            .windows(2)
            .map(|pair| (pair[0] - pair[1]).abs())
            .fold(0.0, f32::max);
        assert_eq!(underruns(&fixture), 1);
        assert!(tail[..20].iter().all(|sample| (*sample - 0.5).abs() < 1e-6));
        assert!(tail[20..116].windows(2).all(|pair| pair[1] < pair[0]));
        assert!(tail[116..].iter().all(|sample| *sample == 0.0));
        assert!(largest_step < 0.5 * PI / 2.0 / 96.0, "step {largest_step}");

        fixture.playout.render(&mut out);
        assert!(silent(&out));
    }

    #[test]
    fn playback_fades_in_when_it_starts_and_after_an_underrun() {
        let mut fixture = fixture_with(declicked(5));
        let mut out = vec![0; 240 * FRAME_BYTES];

        fixture.writer.write(&steady(500, 0.5));
        fixture.playout.render(&mut out);
        let start = left(&out);
        fixture.playout.render(&mut out);
        let steady_state = left(&out);
        fixture.playout.render(&mut out);
        fixture.writer.write(&steady(800, 0.5));
        fixture.playout.render(&mut out);
        let resumed = left(&out);

        for faded in [&start, &resumed] {
            assert!(faded[0] < 0.01);
            assert!(faded[..96].windows(2).all(|pair| pair[1] > pair[0]));
            assert!(
                faded[96..]
                    .iter()
                    .all(|sample| (*sample - 0.5).abs() < 1e-6)
            );
        }
        assert!(
            steady_state
                .iter()
                .all(|sample| (*sample - 0.5).abs() < 1e-6)
        );
    }

    #[test]
    fn the_policy_steps_by_one_codec_frame_and_never_caps_below_the_floor() {
        let config = AudioConfig {
            jitter_buffer_ms: 80,
            jitter_buffer_max_ms: 50,
            jitter_relax_ms: 2_000,
            ..AudioConfig::default()
        };

        let policy = JitterPolicy::from_config(&config);

        assert_eq!(
            policy,
            JitterPolicy {
                floor: Duration::from_millis(80),
                ceiling: Duration::from_millis(80),
                step: Duration::from_millis(5),
                relax: Duration::from_secs(2),
                declick: Duration::from_millis(2),
                widest_correction: 0.005,
            }
        );
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
