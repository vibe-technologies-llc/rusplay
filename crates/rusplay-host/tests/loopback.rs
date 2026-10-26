use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_audio::AudioError;
use rusplay_capture::{CaptureError, CursorBitmap, CursorUpdate, Publisher, Subscriber, latest};
use rusplay_client::{
    AudioOutput, AudioOutputStats, ClientSession, ClientStream, Credentials, LatestFrame,
    PresentedFrame,
};
use rusplay_config::{
    Config, SoftwareDecoderConfig, SvtAv1Config, VaapiConfig, VideoDecoderConfig,
    VideoEncoderConfig,
};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorSpace, PixelFormat, Resolution, Timestamp,
};
use rusplay_host::{
    AudioSource, CaptureFeed, DrmFourcc, Frame, FrameBuffer, FrameSource, Gpu, HostError,
    HostSession, HostStats, InputFeed, InputTarget, PeerStore, Pin,
};
use rusplay_input::{InputError, InputEvent, RumbleCommand};
use tempfile::TempDir;

const PIN: &str = "771205";
const SOURCE: Resolution = Resolution::new(320, 192);
const SOURCE_INTERVAL: Duration = Duration::from_millis(16);
const WAIT: Duration = Duration::from_secs(20);
const GREY: u8 = 128;
const GREY_LINEAR: f32 = 0.2158;
const LINEAR_TOLERANCE: f32 = 0.03;
const BOX_SIZE: u32 = 32;
const FRAMES_TO_SEE: usize = 12;
const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const SVT_AV1_MIN_CHROMA: ChromaSubsampling = ChromaSubsampling::Yuv420;
const TONE_AMPLITUDE: f32 = 0.5;
const TONE_STEP: f32 = 0.05;
const AUDIO_SAMPLES_TO_HEAR: usize = 48_000;
const CURSOR_TARGET_X: i32 = 77;
const CURSOR_SIZE: u32 = 4;
const CURSOR_FILL: u8 = 211;
const HELD_KEY: u32 = 30;

#[derive(Clone, Default)]
struct RecordedInput(Arc<Mutex<Vec<InputEvent>>>);

impl InputTarget for RecordedInput {
    fn inject(&mut self, event: InputEvent) -> Result<(), InputError> {
        self.0.lock().push(event);
        Ok(())
    }

    fn service(&mut self, _rumble: &mut Vec<RumbleCommand>) -> Result<(), InputError> {
        Ok(())
    }
}

struct MovingBox {
    index: u32,
    cursor: Subscriber<CursorUpdate>,
}

struct Tone {
    next: Instant,
    interval: Duration,
    phase: f32,
}

impl AudioSource for Tone {
    fn read_frame(&mut self, frame: &mut [f32], timeout: Duration) -> Result<bool, AudioError> {
        let wait = self.next.saturating_duration_since(Instant::now());
        if wait > timeout {
            thread::sleep(timeout);
            return Ok(false);
        }
        thread::sleep(wait);

        for pair in frame.as_chunks_mut::<2>().0 {
            pair.fill((self.phase * TONE_STEP).sin() * TONE_AMPLITUDE);
            self.phase += 1.0;
        }
        self.next += self.interval;

        Ok(true)
    }
}

#[derive(Clone, Default)]
struct HeardAudio {
    samples: Arc<Mutex<Vec<f32>>>,
}

impl AudioOutput for HeardAudio {
    fn submit(&mut self, pcm: &[f32]) -> Result<usize, AudioError> {
        self.samples.lock().extend_from_slice(pcm);
        Ok(0)
    }

    fn stats(&self) -> AudioOutputStats {
        AudioOutputStats::default()
    }
}

fn cursor_update(x: i32, bitmap: Option<CursorBitmap>) -> CursorUpdate {
    CursorUpdate {
        x,
        y: x / 2,
        visible: true,
        bitmap,
    }
}

fn cursor_bitmap() -> CursorBitmap {
    CursorBitmap {
        width: CURSOR_SIZE,
        height: CURSOR_SIZE,
        hotspot_x: 1,
        hotspot_y: 2,
        rgba: vec![CURSOR_FILL; (CURSOR_SIZE * CURSOR_SIZE * 4) as usize],
    }
}

fn publish(publisher: &Publisher<CursorUpdate>, update: CursorUpdate) {
    publisher.merge(update, CursorUpdate::absorb);
}

impl FrameSource for MovingBox {
    fn cursor_feed(&self) -> Option<Subscriber<CursorUpdate>> {
        Some(self.cursor.clone())
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        thread::sleep(SOURCE_INTERVAL.min(timeout));
        self.index += 1;

        let stride = SOURCE.width * 4;
        let mut data = vec![GREY; (stride * SOURCE.height) as usize];
        let left = (self.index * 4) % (SOURCE.width - BOX_SIZE);
        for y in 0..BOX_SIZE {
            for x in left..left + BOX_SIZE {
                let at = (y * stride + x * 4) as usize;
                data[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }

        Ok(Some(Frame {
            resolution: SOURCE,
            format: DrmFourcc::from_code(*b"XR24"),
            color: ColorSpace::default(),
            timestamp: Timestamp::from_micros(u64::from(self.index) * 16_000),
            buffer: FrameBuffer::Shm { data, stride },
        }))
    }
}

fn host_config(encoder: VideoEncoderConfig) -> Config {
    let mut config = Config::default();
    config.network.bind_address = LOCALHOST;
    config.network.port = 0;
    config.video.codec = Codec::Av1;
    config.video.pixel_format = PixelFormat {
        chroma: SVT_AV1_MIN_CHROMA,
        depth: BitDepth::Eight,
    };
    config.video.encoder = encoder;
    config
}

fn client_config() -> Config {
    let mut config = Config::default();
    config.network.bind_address = LOCALHOST;
    config.video.codec = Codec::H264;
    config.video.decoder = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());
    config.video.decode_queue_frames = u8::MAX;
    config
}

fn supported(config: &Config) -> bool {
    rusplay_encode::capabilities(&config.video.encoder)
        .is_ok_and(|caps| caps.supports(config.video.codec, config.video.pixel_format))
}

fn bind_host(config: Config, dir: &TempDir, input: &RecordedInput) -> Arc<HostSession> {
    let store = PeerStore::open(dir.path().join("paired-clients")).unwrap();
    let session = HostSession::bind(config, store)
        .unwrap()
        .with_input(InputFeed::new(input.clone()))
        .with_audio_source(Arc::new(|config| {
            let samples = config.opus.frame_duration.samples_at(config.sample_rate);
            let nanos = samples as u64 * 1_000_000_000 / u64::from(config.sample_rate.hz());

            Ok(Box::new(Tone {
                next: Instant::now(),
                interval: Duration::from_nanos(nanos),
                phase: 0.0,
            }) as Box<dyn AudioSource>)
        }));

    Arc::new(session)
}

fn serve_in_background(
    host: &Arc<HostSession>,
    gpu: Gpu,
    feed: Arc<CaptureFeed>,
) -> thread::JoinHandle<Result<HostStats, HostError>> {
    let host = Arc::clone(host);

    thread::spawn(move || {
        let client = host.accept(Some(Pin::parse(PIN).unwrap()))?;
        let stream = host.serve(&client, gpu, &feed)?;
        let deadline = Instant::now() + WAIT;
        while !stream.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let stats = stream.stats();
        stream.wait().map(|()| stats)
    })
}

fn loudest(samples: &Mutex<Vec<f32>>) -> f32 {
    samples
        .lock()
        .iter()
        .fold(0.0, |peak: f32, sample| peak.max(sample.abs()))
}

fn press_held_key(stream: &ClientStream, injected: &RecordedInput, deadline: Instant) {
    stream.input().send(InputEvent::Key {
        keycode: HELD_KEY,
        pressed: true,
    });

    while Instant::now() < deadline && injected.0.lock().is_empty() {
        thread::sleep(Duration::from_millis(5));
    }
}

fn assert_held_key_pressed_then_released(injected: &[InputEvent]) {
    let key = |pressed| InputEvent::Key {
        keycode: HELD_KEY,
        pressed,
    };

    assert_eq!(injected, [key(true), key(false)]);
}

fn stream_through_the_real_client(encoder: VideoEncoderConfig) {
    let config = host_config(encoder);
    if !supported(&config) {
        return;
    }
    let Ok(gpu) = Gpu::open() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let injected = RecordedInput::default();
    let host = bind_host(config, &dir, &injected);
    let address = host.local_addr().unwrap();
    let (publisher, subscriber) = latest();
    publish(&publisher, cursor_update(1, Some(cursor_bitmap())));
    let feed = CaptureFeed::new(MovingBox {
        index: 0,
        cursor: subscriber,
    });
    let serving = serve_in_background(&host, gpu.clone(), feed);

    let seen = Arc::new(Mutex::new(Vec::new()));
    let latest = LatestFrame::new();
    let sink = {
        let seen = Arc::clone(&seen);
        let latest = latest.clone();
        move |frame: PresentedFrame| {
            seen.lock()
                .push((frame.frame, frame.keyframe, frame.resolution()));
            rusplay_client::FrameSink::present(&mut latest.clone(), frame);
        }
    };
    let client_gpu = rusplay_client::Gpu {
        device: gpu.device.clone(),
        queue: gpu.queue.clone(),
    };
    let heard = HeardAudio::default();
    let session = ClientSession::new(client_config());
    let stream = session
        .connect_with_audio(
            address,
            &Credentials::Pin(Pin::parse(PIN).unwrap()),
            client_gpu,
            sink,
            Some(Box::new(heard.clone())),
        )
        .unwrap();
    let cursor = stream.cursor();

    let deadline = Instant::now() + WAIT;
    let mut step = 0;
    while Instant::now() < deadline
        && (seen.lock().len() < FRAMES_TO_SEE
            || heard.samples.lock().len() < AUDIO_SAMPLES_TO_HEAR
            || cursor
                .latest()
                .is_none_or(|state| state.x != CURSOR_TARGET_X))
    {
        step = (step + 1).min(CURSOR_TARGET_X);
        publish(&publisher, cursor_update(step, None));
        thread::sleep(Duration::from_millis(5));
    }
    press_held_key(&stream, &injected, deadline);
    let frame = latest.take().unwrap();
    let texels = download(&gpu, &frame.texture);
    drop(frame);
    let client_stats = stream.stats();
    let cursor_state = cursor.latest().unwrap();
    stream.stop().unwrap();
    let host_stats = serving.join().unwrap().unwrap();
    let loudest = loudest(&heard.samples);
    let injected = injected.0.lock().clone();

    let seen = seen.lock().clone();
    let corner = texels[(SOURCE.height as usize - 1) * SOURCE.width as usize];

    assert!(seen.len() >= FRAMES_TO_SEE);
    assert!(
        seen[0].1
            || client_stats.frames_superseded + client_stats.frames_unpresented
                >= u64::from(seen[0].0)
    );
    assert!(host_stats.keyframes_sent >= 1);
    assert!(seen.iter().all(|(_, _, resolution)| *resolution == SOURCE));
    assert!(seen.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(host_stats.frames_sent >= FRAMES_TO_SEE as u64);
    assert!(host_stats.audio_packets_sent >= client_stats.audio_packets_received);
    assert!(client_stats.audio_packets_received > 0);
    assert_eq!(client_stats.audio_decode_errors, 0);
    assert!(loudest > TONE_AMPLITUDE / 2.0);
    assert_held_key_pressed_then_released(&injected);
    assert!(host_stats.input_events >= 1);
    assert_eq!(cursor_state.x, CURSOR_TARGET_X);
    assert_eq!(cursor_state.y, CURSOR_TARGET_X / 2);
    let image = cursor_state.image.unwrap();
    assert_eq!((image.width, image.height), (4, 4));
    assert!(image.rgba.iter().all(|byte| *byte == CURSOR_FILL));
    for channel in &corner[..3] {
        assert!(
            (channel - GREY_LINEAR).abs() < LINEAR_TOLERANCE,
            "grey arrived as {corner:?}"
        );
    }
}

#[test]
fn svt_av1_frames_reach_the_client_session() {
    stream_through_the_real_client(VideoEncoderConfig::SvtAv1(SvtAv1Config::default()));
}

#[test]
fn vaapi_frames_reach_the_client_session() {
    stream_through_the_real_client(VideoEncoderConfig::Vaapi(VaapiConfig::default()));
}

fn download(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (width, height) = (texture.width(), texture.height());
    let texel_bytes = 8;
    let row_bytes = (width * texel_bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rusplay-host-test-readback"),
        size: u64::from(row_bytes * height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);

    staging.map_async(wgpu::MapMode::Read, .., |result| {
        result.expect("readback buffer maps");
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device finishes the readback");
    let bytes = staging
        .get_mapped_range(..)
        .expect("mapped range is readable")
        .to_vec();

    bytes
        .chunks_exact(row_bytes as usize)
        .flat_map(|row| {
            row[..(width * texel_bytes) as usize]
                .as_chunks::<8>()
                .0
                .to_vec()
        })
        .map(|texel| {
            std::array::from_fn(|channel| {
                f16_to_f32(u16::from_le_bytes([
                    texel[2 * channel],
                    texel[2 * channel + 1],
                ]))
            })
        })
        .collect()
}

fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = f32::from(bits & 0x3ff);

    sign * match exponent {
        0 => mantissa * 2f32.powi(-24),
        _ => (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
    }
}
