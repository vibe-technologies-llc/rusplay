use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, Thread},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use pipewire::{
    context::ContextRc,
    keys,
    main_loop::MainLoopRc,
    properties::properties,
    spa::{pod::Pod, utils::Direction},
    stream::{StreamBox, StreamFlags, StreamState},
};
use rtrb::{Consumer, Producer, RingBuffer};
use rusplay_config::AudioConfig;
use rusplay_core::{AudioCodec, ChannelLayout};

use crate::{
    AudioError, PcmResampler,
    pw_common::{PipewireThread, StartupSender, format_pod_bytes, negotiated_format, node_latency},
};

const RING_MILLISECONDS: usize = 250;
const SCRATCH_SAMPLES: usize = 16 * 1024;
const BYTES_PER_SAMPLE: usize = size_of::<f32>();

type SharedWaiter = Arc<Mutex<Option<Thread>>>;

fn frames_per_ring(rate: u32) -> usize {
    rate as usize * RING_MILLISECONDS / 1000
}

fn wake(waiter: &SharedWaiter) {
    if let Some(guard) = waiter.try_lock()
        && let Some(thread) = guard.as_ref()
    {
        thread.unpark();
    }
}

struct RingWriter {
    producer: Producer<f32>,
    channels: usize,
}

impl RingWriter {
    fn write(&mut self, samples: &[f32]) -> usize {
        let writable = self.producer.slots().min(samples.len()) / self.channels * self.channels;
        match self.producer.push_entire_slice(&samples[..writable]) {
            Ok(()) => samples.len() - writable,
            Err(_) => samples.len(),
        }
    }
}

pub(crate) struct SampleSource {
    consumer: Consumer<f32>,
    resampler: PcmResampler,
    channels: usize,
    raw: Vec<f32>,
    ready: VecDeque<f32>,
    converted: Vec<f32>,
}

impl SampleSource {
    fn new(consumer: Consumer<f32>, resampler: PcmResampler, channels: usize) -> Self {
        Self {
            consumer,
            resampler,
            channels,
            raw: Vec::new(),
            ready: VecDeque::new(),
            converted: Vec::new(),
        }
    }

    fn fill(&mut self, frame: &mut [f32]) -> Result<bool, AudioError> {
        let available = self.consumer.slots() / self.channels * self.channels;
        if available > 0 {
            self.raw.resize(available, 0.0);
            self.consumer
                .pop_entire_slice(&mut self.raw)
                .map_err(|_| AudioError::RingUnderflow)?;

            self.converted.clear();
            self.resampler.push(&self.raw, &mut self.converted)?;
            self.ready.extend(self.converted.iter().copied());
        }

        if self.ready.len() < frame.len() {
            return Ok(false);
        }

        let wanted = frame.len();
        for (slot, sample) in frame.iter_mut().zip(self.ready.drain(..wanted)) {
            *slot = sample;
        }
        Ok(true)
    }
}

fn sample_ring(rate: u32, channels: usize) -> (RingWriter, Consumer<f32>) {
    let (producer, consumer) = RingBuffer::new(frames_per_ring(rate) * channels);
    (RingWriter { producer, channels }, consumer)
}

struct CaptureCallbacks {
    writer: RingWriter,
    overruns: Arc<AtomicU64>,
    waiter: SharedWaiter,
    scratch: Vec<f32>,
    startup: Option<StartupSender>,
    channels: usize,
}

struct CaptureSettings {
    layout: ChannelLayout,
    rate: u32,
    latency_frames: usize,
}

fn latency_frames(config: &AudioConfig) -> usize {
    match config.codec {
        AudioCodec::Opus => config.opus.frame_duration.samples_at(config.sample_rate),
        AudioCodec::Flac => usize::from(config.flac.block_size),
    }
}

fn run_capture(
    settings: &CaptureSettings,
    mut callbacks: CaptureCallbacks,
    quit: pipewire::channel::Receiver<()>,
) -> Result<(), AudioError> {
    pipewire::init();

    let mainloop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Capture",
        *keys::MEDIA_ROLE => "Music",
        *keys::NODE_NAME => "rusplay-capture",
        *keys::STREAM_CAPTURE_SINK => "true",
    };
    props.insert(
        *keys::NODE_LATENCY,
        node_latency(settings.latency_frames, settings.rate),
    );

    let stream = StreamBox::new(&core, "rusplay-capture", props)?;

    callbacks.scratch = Vec::with_capacity(SCRATCH_SAMPLES);

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
        .state_changed(|_, state, _, new| {
            if let StreamState::Error(reason) = new
                && let Some(startup) = state.startup.take()
            {
                let _ = startup.send(Err(AudioError::PipeWireStream { reason }));
            }
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };

            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            let Some(mapped) = data.data() else {
                return;
            };
            let end = (offset + size).min(mapped.len());
            let Some(payload) = mapped.get(offset.min(end)..end) else {
                return;
            };

            state.scratch.clear();
            let (samples, _) = payload.as_chunks::<BYTES_PER_SAMPLE>();
            state
                .scratch
                .extend(samples.iter().map(|bytes| f32::from_le_bytes(*bytes)));

            let dropped = state.writer.write(&state.scratch);
            if dropped > 0 {
                state.overruns.fetch_add(dropped as u64, Ordering::Relaxed);
            }
            wake(&state.waiter);
        })
        .register()?;

    let pod_bytes = format_pod_bytes(settings.layout, settings.rate)?;
    let pod = Pod::from_bytes(&pod_bytes).ok_or(AudioError::PipeWireFormatEncode)?;

    stream.connect(
        Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut [pod],
    )?;

    let loop_handle = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| loop_handle.quit());

    mainloop.run();
    Ok(())
}

pub struct AudioCapture {
    source: SampleSource,
    thread: PipewireThread,
    waiter: SharedWaiter,
    overruns: Arc<AtomicU64>,
    native_rate: u32,
}

impl AudioCapture {
    pub fn start(config: &AudioConfig) -> Result<Self, AudioError> {
        let settings = CaptureSettings {
            layout: config.channels,
            rate: config.sample_rate.hz(),
            latency_frames: latency_frames(config),
        };
        let channels = usize::from(config.channels.channel_count());

        let (writer, consumer) = sample_ring(settings.rate, channels);
        let overruns = Arc::new(AtomicU64::new(0));
        let waiter: SharedWaiter = Arc::new(Mutex::new(None));

        let callbacks = CaptureCallbacks {
            writer,
            overruns: Arc::clone(&overruns),
            waiter: Arc::clone(&waiter),
            scratch: Vec::new(),
            startup: None,
            channels,
        };

        let (thread, native) =
            PipewireThread::spawn("rusplay-audio-capture", move |quit, startup| {
                let mut callbacks = callbacks;
                callbacks.startup = Some(startup);
                run_capture(&settings, callbacks, quit)
            })?;

        let resampler = PcmResampler::new(
            native.rate,
            config.sample_rate.hz(),
            config.channels.channel_count(),
        )?;

        Ok(Self {
            source: SampleSource::new(consumer, resampler, channels),
            thread,
            waiter,
            overruns,
            native_rate: native.rate,
        })
    }

    pub fn native_rate(&self) -> u32 {
        self.native_rate
    }

    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    pub fn try_read_frame(&mut self, frame: &mut [f32]) -> Result<bool, AudioError> {
        self.source.fill(frame)
    }

    pub fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, AudioError> {
        *self.waiter.lock() = Some(thread::current());
        let deadline = Instant::now() + timeout;

        loop {
            if self.source.fill(frame)? {
                return Ok(true);
            }
            if !self.thread.is_running() {
                return Err(AudioError::PipeWireStopped);
            }

            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            thread::park_timeout(deadline - now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_for(
        native_rate: u32,
        target_rate: u32,
        channels: usize,
    ) -> (RingWriter, SampleSource) {
        let (writer, consumer) = sample_ring(native_rate, channels);
        let resampler =
            PcmResampler::new(native_rate, target_rate, u8::try_from(channels).unwrap()).unwrap();
        (writer, SampleSource::new(consumer, resampler, channels))
    }

    #[test]
    fn frames_come_out_in_order_and_whole() {
        let (mut writer, mut source) = source_for(48_000, 48_000, 2);
        let input: Vec<f32> = (0..1000u16).map(f32::from).collect();

        assert_eq!(writer.write(&input), 0);

        let mut frame = vec![0.0; 240];
        let mut collected = Vec::new();
        while source.fill(&mut frame).unwrap() {
            collected.extend_from_slice(&frame);
        }

        assert_eq!(collected, input[..960]);
    }

    #[test]
    fn short_data_does_not_produce_a_frame() {
        let (mut writer, mut source) = source_for(48_000, 48_000, 2);
        writer.write(&[0.5; 100]);

        let mut frame = vec![0.0; 480];

        assert!(!source.fill(&mut frame).unwrap());
    }

    #[test]
    fn a_full_ring_reports_whole_frames_dropped() {
        let (mut writer, _source) = source_for(48_000, 48_000, 2);
        let capacity = frames_per_ring(48_000) * 2;
        let flood = vec![0.1; capacity + 7];

        let dropped = writer.write(&flood);

        assert_eq!(dropped, 7);
    }

    #[test]
    fn writes_never_split_a_frame() {
        let (mut writer, _source) = source_for(48_000, 48_000, 6);
        let capacity = frames_per_ring(48_000) * 6;
        let flood = vec![0.1; capacity + 5];

        let dropped = writer.write(&flood);

        assert_eq!(dropped % 6, 5);
    }

    #[test]
    fn other_rates_are_converted_to_the_target() {
        let (mut writer, mut source) = source_for(44_100, 48_000, 2);
        let input = vec![0.25; 44_100 / 5 * 2];
        writer.write(&input);

        let mut frame = vec![0.0; 480];
        let mut frames = 0;
        while source.fill(&mut frame).unwrap() {
            frames += 1;
        }

        assert!((f64::from(frames) * 240.0 - 9_600.0).abs() < 600.0);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    #[ignore = "needs a running PipeWire session"]
    fn default_sink_monitor_delivers_frames() {
        let config = AudioConfig::default();
        let mut capture = AudioCapture::start(&config).unwrap();

        let frame_samples = config.opus.frame_duration.samples_at(config.sample_rate) * 2;
        let mut frame = vec![0.0; frame_samples];
        let mut delivered = 0;
        for _ in 0..100 {
            if capture
                .read_frame(&mut frame, Duration::from_secs(1))
                .unwrap()
            {
                delivered += 1;
            }
        }

        assert_eq!(capture.native_rate(), config.sample_rate.hz());
        assert!(delivered >= 90, "only {delivered} frames in 100 reads");
    }
}
