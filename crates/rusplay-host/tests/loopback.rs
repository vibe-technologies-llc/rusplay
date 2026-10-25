use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use rusplay_capture::CaptureError;
use rusplay_client::{ClientSession, Credentials, LatestFrame, PresentedFrame};
use rusplay_config::{
    Config, SoftwareDecoderConfig, SvtAv1Config, VaapiConfig, VideoDecoderConfig,
    VideoEncoderConfig,
};
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, ColorSpace, PixelFormat, Resolution, Timestamp,
};
use rusplay_host::{
    CaptureFeed, DrmFourcc, Frame, FrameBuffer, FrameSource, Gpu, HostSession, PeerStore, Pin,
};
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

struct MovingBox {
    index: u32,
}

impl FrameSource for MovingBox {
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
    config.video.codec = Codec::Av1;
    config.video.decoder = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());
    config.video.decode_queue_frames = 16;
    config
}

fn supported(config: &Config) -> bool {
    rusplay_encode::capabilities(&config.video.encoder)
        .is_ok_and(|caps| caps.supports(config.video.codec, config.video.pixel_format))
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
    let store = PeerStore::open(dir.path().join("paired-clients")).unwrap();
    let host = Arc::new(HostSession::bind(config, store).unwrap());
    let address = host.local_addr().unwrap();
    let feed = CaptureFeed::new(MovingBox { index: 0 });

    let serving = {
        let host = Arc::clone(&host);
        let gpu = gpu.clone();
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
    };

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
    let session = ClientSession::new(client_config());
    let stream = session
        .connect(
            address,
            &Credentials::Pin(Pin::parse(PIN).unwrap()),
            client_gpu,
            sink,
        )
        .unwrap();

    let deadline = Instant::now() + WAIT;
    while seen.lock().len() < FRAMES_TO_SEE && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let frame = latest.take().unwrap();
    let texels = download(&gpu, &frame.texture);
    drop(frame);
    stream.stop().unwrap();
    let host_stats = serving.join().unwrap().unwrap();

    let seen = seen.lock().clone();
    let corner = texels[(SOURCE.height as usize - 1) * SOURCE.width as usize];

    assert!(seen.len() >= FRAMES_TO_SEE);
    assert!(seen[0].1);
    assert!(seen.iter().all(|(_, _, resolution)| *resolution == SOURCE));
    assert!(seen.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(host_stats.frames_sent >= FRAMES_TO_SEE as u64);
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
