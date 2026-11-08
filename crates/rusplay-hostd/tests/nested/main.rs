#[path = "../../src/hardware.rs"]
mod hardware;

mod daemon;
mod input;
mod offer;
mod painter;
mod resize;
mod service;
mod stack;

use std::{
    net::SocketAddr,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use daemon::{Daemon, DaemonOptions};
use painter::Painter;
use parking_lot::Mutex;
use rusplay_client::{
    ClientError, ClientSession, ClientStats, ClientStream, ColorVolume, Credentials, FrameColor,
    Gpu, Pin, PresentedFrame, announced_display,
};
use rusplay_config::{Config, SoftwareDecoderConfig, VideoDecoderConfig};
use rusplay_core::{Codec, FrameRate, Resolution};
use stack::{Stack, read_trimmed, wait_for};

static SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const DIALOG_WAIT: Duration = Duration::from_secs(40);
const LISTEN_WAIT: Duration = Duration::from_secs(10);
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(10);

struct Live {
    daemon: Daemon,
    _painter: Painter,
    stack: Stack,
    address: SocketAddr,
}

impl Live {
    fn start(animate: bool, extra: &str) -> Option<Self> {
        let stack = Stack::start()?;
        let config = Daemon::write_config(
            &stack,
            &DaemonOptions {
                extra: extra.to_owned(),
                ..DaemonOptions::default()
            },
        );
        let daemon = Daemon::start(&stack, config);

        let pin_path = Daemon::pin_path(&stack);
        let answered = stack.answer_dialog_until(|| pin_path.exists(), DIALOG_WAIT);
        assert!(
            answered,
            "the portal dialog was not answered, hostd said: {:#?}",
            daemon.lines()
        );
        let address = daemon.address(LISTEN_WAIT).expect("hostd never listened");
        let painter = Painter::start(&stack, animate);

        Some(Self {
            daemon,
            _painter: painter,
            stack,
            address,
        })
    }

    fn pin(&self) -> Pin {
        let text = read_trimmed(&Daemon::pin_path(&self.stack)).expect("no pin file");
        Pin::parse(&text).unwrap()
    }
}

#[derive(Debug, Clone)]
struct Seen {
    at: Instant,
    keyframe: bool,
    source: Resolution,
    color: FrameColor,
}

type Recording = Arc<Mutex<Vec<Seen>>>;

fn recorder() -> (Recording, impl FnMut(PresentedFrame) + Send + 'static) {
    let seen: Recording = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let seen = Arc::clone(&seen);
        move |frame: PresentedFrame| {
            seen.lock().push(Seen {
                at: Instant::now(),
                keyframe: frame.keyframe,
                source: frame.source,
                color: frame.color,
            });
        }
    };

    (seen, sink)
}

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let found = pollster::block_on(async {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..wgpu::RequestAdapterOptions::default()
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()?;

        Some(Gpu::new(device, queue))
    });

    if found.is_none() {
        hardware::missing("a wgpu adapter and device");
    }

    found
}

fn client_config() -> Config {
    let mut config = Config::default();
    config.video.codec = rusplay_config::CodecChoice::pinned(Codec::Av1);
    config.video.decoder = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());
    config.network.bind_address = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    config.input.gamepads = false;
    config.video.decode_queue_frames = u8::MAX;

    config
}

fn connect(
    live: &Live,
    gpu: Gpu,
    credentials: &Credentials,
    display: (u32, u32, u32),
    sink: impl FnMut(PresentedFrame) + Send + 'static,
) -> Result<ClientStream, ClientError> {
    let (width, height, millihertz) = display;
    let announced = announced_display(
        Resolution::new(width, height),
        FrameRate::new(millihertz, 1000),
        &ColorVolume::srgb_display(),
    )
    .unwrap();

    ClientSession::new(client_config())
        .with_display(announced)
        .connect_with_audio(live.address, credentials, gpu, sink, None)
}

#[derive(Debug)]
struct Observation {
    asked: (u32, u32, u32),
    first: Option<Seen>,
    sizes: Vec<Resolution>,
    frames: usize,
    per_second: Vec<usize>,
    colors: Vec<FrameColor>,
    keyframe_times: Vec<(u64, Resolution)>,
    size_changes: Vec<(u64, Resolution)>,
    client: ClientStats,
    host: Vec<String>,
}

impl Observation {
    fn report(&self) {
        let sizes: Vec<String> = self
            .size_changes
            .iter()
            .map(|(at, size)| format!("{}x{}@{at}ms", size.width, size.height))
            .collect();
        let keys: Vec<String> = self
            .keyframe_times
            .iter()
            .map(|(at, _)| format!("{at}"))
            .collect();
        eprintln!("REPORT asked {:?}", self.asked);
        eprintln!("REPORT sizes {sizes:?}");
        eprintln!("REPORT keyframes at ms {keys:?}");
        eprintln!(
            "REPORT frames {} per second {:?}",
            self.frames,
            &self.per_second[..8.min(self.per_second.len())]
        );
        eprintln!(
            "REPORT client keyframe_requests {} recovery_requests {} skipped {} lost {} decode_errors {} superseded {} stream_formats {}",
            self.client.keyframe_requests_sent,
            self.client.recovery_requests_sent,
            self.client.frames_skipped,
            self.client.frames_lost,
            self.client.decode_errors,
            self.client.frames_superseded,
            self.client.stream_formats_received
        );
        eprintln!(
            "REPORT client dropped {} packets_missing {} recovered {} reference_recoveries {} received {} decoded {}",
            self.client.frames_dropped,
            self.client.packets_missing,
            self.client.frames_recovered,
            self.client.reference_recoveries,
            self.client.frames_received,
            self.client.frames_decoded
        );
        eprintln!("REPORT colors {}", self.colors.len());
        for line in self
            .host
            .iter()
            .filter(|line| line.contains("rusplay-hostd"))
        {
            eprintln!("REPORT host {}", &line[..line.len().min(170)]);
        }
    }
}

fn observe(display: (u32, u32, u32), animate: bool, seconds: u64) -> Option<Observation> {
    observe_with(display, animate, seconds, "")
}

fn observe_with(
    display: (u32, u32, u32),
    animate: bool,
    seconds: u64,
    extra: &str,
) -> Option<Observation> {
    let _serial = SERIAL.lock();
    let live = Live::start(animate, extra)?;
    let gpu = gpu()?;

    let (seen, sink) = recorder();
    let started = Instant::now();
    let stream = connect(&live, gpu, &Credentials::Pin(live.pin()), display, sink).unwrap();
    wait_for(FIRST_FRAME_WAIT, || !seen.lock().is_empty());
    std::thread::sleep(Duration::from_secs(seconds));
    let client = stream.stats();
    stream.stop().unwrap();

    let seen = seen.lock().clone();
    let mut sizes: Vec<Resolution> = Vec::new();
    for entry in &seen {
        if sizes.last() != Some(&entry.source) {
            sizes.push(entry.source);
        }
    }
    let mut colors: Vec<FrameColor> = Vec::new();
    for entry in &seen {
        if colors.last() != Some(&entry.color) {
            colors.push(entry.color);
        }
    }
    let mut per_second = vec![0; usize::try_from(seconds).unwrap() + 12];
    for entry in &seen {
        let bucket = usize::try_from(entry.at.duration_since(started).as_secs()).unwrap();
        if let Some(count) = per_second.get_mut(bucket) {
            *count += 1;
        }
    }

    let millis =
        |entry: &Seen| u64::try_from(entry.at.duration_since(started).as_millis()).unwrap();
    let keyframe_times = seen
        .iter()
        .filter(|entry| entry.keyframe)
        .map(|entry| (millis(entry), entry.source))
        .collect();
    let mut size_changes: Vec<(u64, Resolution)> = Vec::new();
    for entry in &seen {
        if size_changes.last().map(|(_, size)| size) != Some(&entry.source) {
            size_changes.push((millis(entry), entry.source));
        }
    }

    Some(Observation {
        keyframe_times,
        size_changes,
        client,
        asked: display,
        first: seen.first().cloned(),
        sizes,
        frames: seen.len(),
        per_second,
        colors,
        host: live.daemon.lines(),
    })
}
