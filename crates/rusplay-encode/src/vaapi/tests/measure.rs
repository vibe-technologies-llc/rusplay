use std::time::{Duration, Instant};

use rusplay_config::FrameRateChoice;
use rusplay_core::{FrameRate, QualityScore, SamplePicture, SamplePlane, TemporalOrder, measure};

use super::*;
use crate::{input::YuvFrame, vaapi::coded_extent};

const LOOP_FRAMES: usize = 16;
const WARMUP_FRAMES: usize = 30;
const MEASURED_FRAMES: usize = 630;
const SAMPLE_INTERVAL: usize = 5;
const FRAME_RATE: u32 = 60;
const FULL_HD: Resolution = Resolution::new(1920, 1080);
const ULTRA_HD: Resolution = Resolution::new(3840, 2160);
const FULL_HD_KBPS: u32 = 20_000;
const ULTRA_HD_KBPS: u32 = 80_000;
const COSINES: [i64; LOOP_FRAMES] = [
    1024, 946, 724, 392, 0, -392, -724, -946, -1024, -946, -724, -392, 0, 392, 724, 946,
];
const QUARTER_TURN: usize = LOOP_FRAMES / 4;
const BLACK: u64 = 16;
const WHITE: u64 = 235;
const OBJECTS: usize = 3;
const GRAIN_SEED: u64 = 1000;
const GRAIN_LEVELS: u64 = 3;

fn lattice(x: usize, y: usize, seed: u64) -> u64 {
    let mut value = (x as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (y as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
        ^ seed.wrapping_mul(0x1656_67b1_9e37_79f9);
    value ^= value >> 29;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 32;

    value & 0xff
}

fn octave(x: usize, y: usize, cell: usize, seed: u64) -> u64 {
    let (column, row) = (x / cell, y / cell);
    let (across, down) = ((x % cell) as u64, (y % cell) as u64);
    let cell = cell as u64;

    let top =
        lattice(column, row, seed) * (cell - across) + lattice(column + 1, row, seed) * across;
    let bottom = lattice(column, row + 1, seed) * (cell - across)
        + lattice(column + 1, row + 1, seed) * across;

    (top * (cell - down) + bottom * down) / (cell * cell)
}

fn textured(x: usize, y: usize) -> u64 {
    let base = (octave(x, y, 128, 1) * 6 + octave(x, y, 32, 2) * 3 + octave(x, y, 8, 3) * 2) / 11;
    let fine = octave(x, y, 2, 4);
    let amount = octave(x, y, 256, 5);

    ((base * 768 + fine * amount).saturating_sub(128 * amount) / 768).clamp(BLACK, WHITE)
}

fn tinted(x: usize, y: usize, seed: u64) -> u64 {
    88 + (octave(x, y, 64, seed) * 2 + octave(x, y, 16, seed + 1)) * 80 / (3 * 255)
}

struct Canvas {
    width: usize,
    samples: Vec<u8>,
}

impl Canvas {
    fn new(width: usize, height: usize, value: impl Fn(usize, usize) -> u64) -> Self {
        let samples = (0..height)
            .flat_map(|y| {
                let value = &value;
                (0..width).map(move |x| u8::try_from(value(x, y)).unwrap_or(u8::MAX))
            })
            .collect();

        Self { width, samples }
    }

    fn at(&self, x: usize, y: usize) -> u64 {
        u64::from(self.samples[y * self.width + x])
    }
}

fn orbit(radius: usize, step: usize) -> (usize, usize) {
    let reach = i64::try_from(radius).unwrap();
    let offset = |cosine: i64| usize::try_from(reach + reach * cosine / 1024).unwrap();

    (
        offset(COSINES[step % LOOP_FRAMES]),
        offset(COSINES[(step + LOOP_FRAMES - QUARTER_TURN) % LOOP_FRAMES]),
    )
}

#[derive(Debug, Clone, Copy)]
enum Layer {
    Hud,
    Object {
        number: usize,
        across: usize,
        down: usize,
    },
    Sky {
        weight: u64,
    },
    Veil,
    Ground,
}

struct Scene {
    resolution: Resolution,
    radius: usize,
    luma: Canvas,
    cb: Canvas,
    cr: Canvas,
}

impl Scene {
    fn new(resolution: Resolution) -> Self {
        let (width, height) = (resolution.width as usize, resolution.height as usize);
        let radius = height / 36;
        let margin = 2 * radius + 1;

        Self {
            resolution,
            radius,
            luma: Canvas::new(width + margin, height + margin, textured),
            cb: Canvas::new(width / 2 + margin, height / 2 + margin, |x, y| {
                tinted(x, y, 6)
            }),
            cr: Canvas::new(width / 2 + margin, height / 2 + margin, |x, y| {
                tinted(x, y, 8)
            }),
        }
    }

    fn size(&self) -> (usize, usize) {
        (
            self.resolution.width as usize,
            self.resolution.height as usize,
        )
    }

    fn object_side(&self) -> usize {
        self.size().1 / 6
    }

    fn object_corner(&self, number: usize, index: usize) -> (usize, usize) {
        let (width, height) = self.size();
        let (reach, speed, phase) = [(2, 1, 8), (1, 2, 0), (3, 1, 4)][number];
        let (across, down) = orbit(reach * self.radius, index * speed + phase);

        (
            width * (number + 1) / 4 - self.object_side() / 2 + across,
            height * 3 / 5 - self.object_side() / 2 + down,
        )
    }

    fn layer(&self, x: usize, y: usize, index: usize) -> Layer {
        let (width, height) = self.size();
        let horizon = height / 3;
        let side = self.object_side();

        if x < width / 5 && y >= height - height / 8 {
            return Layer::Hud;
        }
        for number in 0..OBJECTS {
            let (left, top) = self.object_corner(number, index);
            if (left..left + side).contains(&x) && (top..top + side).contains(&y) {
                return Layer::Object {
                    number,
                    across: x - left,
                    down: y - top,
                };
            }
        }
        if y < horizon {
            return Layer::Sky {
                weight: ((horizon - y) * 256 / horizon) as u64,
            };
        }
        if x >= width * 3 / 5 {
            return Layer::Veil;
        }

        Layer::Ground
    }

    fn luma_at(&self, x: usize, y: usize, index: usize) -> u64 {
        let (width, height) = self.size();
        let (pan_across, pan_down) = orbit(self.radius, index);
        let ground = self.luma.at(x + pan_across, y + pan_down);
        let grained = |value: u64| value + lattice(x, y, GRAIN_SEED + index as u64) % GRAIN_LEVELS;

        match self.layer(x, y, index) {
            Layer::Hud if (x / 3 + y / 12).is_multiple_of(2) => 220,
            Layer::Hud => 30,
            Layer::Object {
                number,
                across,
                down,
            } => WHITE + BLACK - self.luma.at(number * 97 + across, number * 61 + down),
            Layer::Sky { weight } => {
                let gradient = (96 + 96 * x / width + 96 * y / height) as u64;
                (gradient * weight + ground * (256 - weight)) / 256
            }
            Layer::Veil => {
                let (drift_across, drift_down) = orbit(self.radius, index + LOOP_FRAMES / 2);
                let veil = self.luma.at(width - 1 - x + drift_across, y + drift_down);
                grained(ground.midpoint(veil))
            }
            Layer::Ground => grained(ground),
        }
    }

    fn chroma_at(&self, canvas: &Canvas, tint: u64, x: usize, y: usize, index: usize) -> u64 {
        let (pan_across, pan_down) = orbit(self.radius, index);
        let ground = canvas.at(x + pan_across / 2, y + pan_down / 2);

        match self.layer(2 * x, 2 * y, index) {
            Layer::Hud => 128,
            Layer::Object { number, .. } => tint + 24 * number as u64,
            Layer::Sky { weight } => ((tint + 40) * weight + ground * (256 - weight)) / 256,
            Layer::Veil | Layer::Ground => ground,
        }
    }

    fn frame(&self, index: usize) -> Frame {
        let (width, height) = self.size();
        let sample = |value: u64| u8::try_from(value).unwrap_or(u8::MAX);
        let luma =
            (0..height).flat_map(|y| (0..width).map(move |x| sample(self.luma_at(x, y, index))));
        let chroma = [(&self.cb, 96), (&self.cr, 104)]
            .into_iter()
            .flat_map(|(canvas, tint)| {
                (0..height / 2).flat_map(move |y| {
                    (0..width / 2).map(move |x| sample(self.chroma_at(canvas, tint, x, y, index)))
                })
            });

        Frame {
            resolution: self.resolution,
            format: fourcc_for(yuv420(BitDepth::Eight)),
            timestamp: timestamp(index as u64),
            color: ColorSpace::default(),
            buffer: FrameBuffer::Shm {
                data: luma.chain(chroma).collect(),
                stride: self.resolution.width,
            },
        }
    }
}

struct Source {
    _surface: Surface,
    frame: Frame,
    picture: SamplePicture,
}

fn source(display: &Rc<Display>, scene: &Scene, index: usize) -> Source {
    let format = yuv420(BitDepth::Eight);
    let planar = scene.frame(index);
    let view = YuvFrame::from_frame(&planar, crate::Backend::Vaapi, format).unwrap();
    let surface_format = surface_format(format);
    let extent = coded_extent(scene.resolution);

    let surface = exportable_surface(display, surface_format, extent).unwrap();
    let mut image = Image::new(display, surface_format.va_fourcc, extent).unwrap();
    image
        .write(|data, layout| pack(&view, data, layout, extent))
        .unwrap();
    image.put(&surface).unwrap();
    display.sync(surface.id()).unwrap();

    let exported = export_surface(&surface).unwrap();
    let frame = Frame {
        resolution: scene.resolution,
        format: DrmFourcc(surface_format.drm_fourcc),
        timestamp: planar.timestamp,
        color: ColorSpace::default(),
        buffer: FrameBuffer::DmaBuf {
            fd: exported.fd,
            modifier: exported.modifier,
            planes: exported
                .planes
                .iter()
                .map(|plane| Plane {
                    offset: plane.offset,
                    stride: plane.pitch,
                })
                .collect(),
            lease: BufferLease::detached(),
        },
    };

    Source {
        _surface: surface,
        frame,
        picture: view.sample_picture().unwrap(),
    }
}

struct Variant {
    name: String,
    params: Vec<(&'static str, String)>,
}

impl Variant {
    fn config(&self, codec: Codec, resolution: Resolution, bitrate_kbps: u32) -> VideoConfig {
        let config = VideoConfig {
            bitrate_kbps,
            frame_rate: FrameRateChoice::Fixed(FrameRate::integer(FRAME_RATE)),
            ..video(codec, yuv420(BitDepth::Eight), resolution)
        };

        with_vaapi(config, |vaapi| {
            vaapi.params = self
                .params
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect();
        })
    }
}

#[derive(Default)]
struct Tally {
    times: Vec<Duration>,
    bytes: usize,
    frames: usize,
    scores: Vec<QualityScore>,
}

fn count(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap())
}

impl Tally {
    fn percentile_ms(&self, percent: usize) -> f64 {
        let mut times = self.times.clone();
        times.sort_unstable();

        times[(times.len() - 1) * percent / 100].as_secs_f64() * 1000.0
    }

    fn kbps(&self) -> f64 {
        count(self.bytes) * 8.0 * f64::from(FRAME_RATE) / count(self.frames) / 1000.0
    }

    fn mean(&self, decibels: impl Fn(&QualityScore) -> f32) -> f64 {
        let total: f64 = self
            .scores
            .iter()
            .map(|score| f64::from(decibels(score)))
            .sum();

        total / count(self.scores.len())
    }
}

struct Shuffler(u64);

impl Shuffler {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;

        self.0
    }

    fn order(&mut self, count: usize) -> Vec<usize> {
        let mut order: Vec<usize> = (0..count).collect();
        for last in (1..count).rev() {
            let pick = usize::try_from(self.next() % (last as u64 + 1)).unwrap();
            order.swap(last, pick);
        }

        order
    }
}

fn score_all(
    source: &SamplePicture,
    history: [&SamplePlane; 2],
    reconstructions: &[SamplePicture],
) -> Vec<QualityScore> {
    let order = TemporalOrder::for_rate(FrameRate::integer(FRAME_RATE));

    thread::scope(|scope| {
        let workers: Vec<_> = reconstructions
            .iter()
            .map(|reconstruction| {
                scope.spawn(move || measure(source, reconstruction, &history, order).unwrap())
            })
            .collect();

        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect()
    })
}

fn compare(codec: Codec, resolution: Resolution, bitrate_kbps: u32, variants: &[Variant]) {
    if !device_present() {
        return;
    }
    let Some(mut encoders) = variants
        .iter()
        .map(|variant| try_open(&variant.config(codec, resolution, bitrate_kbps)))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let display = Display::open(&VaapiConfig::default().device).unwrap();
    let scene = Scene::new(resolution);
    let sources: Vec<Source> = (0..LOOP_FRAMES)
        .map(|index| source(&display, &scene, index))
        .collect();
    let mut tallies: Vec<Tally> = variants.iter().map(|_| Tally::default()).collect();
    let mut shuffler = Shuffler(0x2545_f491_4f6c_dd1d);

    for index in 0..WARMUP_FRAMES + MEASURED_FRAMES {
        let current = &sources[index % LOOP_FRAMES];
        let measured = index >= WARMUP_FRAMES;
        let sampled = measured && index % SAMPLE_INTERVAL == 0;
        let order = shuffler.order(variants.len());
        let mut reconstructions = Vec::new();

        for &variant in &order {
            let encoder = &mut encoders[variant];
            if sampled {
                assert!(encoder.capture_reconstruction());
            }
            let started = Instant::now();
            let packet = encoder.encode(&current.frame).unwrap().unwrap();
            let elapsed = started.elapsed();

            if !measured {
                continue;
            }
            tallies[variant].bytes += packet.data.len();
            tallies[variant].frames += 1;
            if sampled {
                reconstructions.push(encoder.take_reconstruction().unwrap().unwrap());
            } else {
                tallies[variant].times.push(elapsed);
            }
        }

        if sampled {
            let history = [1, 2].map(|back| {
                sources[(index + LOOP_FRAMES - back) % LOOP_FRAMES]
                    .picture
                    .luma()
            });
            let scores = score_all(&current.picture, history, &reconstructions);
            for (&variant, score) in order.iter().zip(scores) {
                tallies[variant].scores.push(score);
            }
        }
    }

    println!(
        "{codec} {resolution} CBR {bitrate_kbps} kbps at {FRAME_RATE} fps: {} timed and {} scored frames per configuration",
        tallies[0].times.len(),
        tallies[0].scores.len(),
    );
    println!(
        "{:<34} {:>9} {:>9} {:>9} {:>8} {:>8} {:>8} {:>8}",
        "configuration", "median ms", "p95 ms", "kbps", "PSNR-Y", "PSNR", "XPSNR-Y", "XPSNR-C"
    );
    for (variant, tally) in variants.iter().zip(&tallies) {
        println!(
            "{:<34} {:>9.3} {:>9.3} {:>9.0} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
            variant.name,
            tally.percentile_ms(50),
            tally.percentile_ms(95),
            tally.kbps(),
            tally.mean(|score| score.psnr.luma.value()),
            tally.mean(|score| score.psnr_overall.value()),
            tally.mean(|score| score.xpsnr.luma.value()),
            tally.mean(|score| f32::midpoint(score.xpsnr.cb.value(), score.xpsnr.cr.value())),
        );
    }
}

fn measured_cases() -> Vec<(Codec, Resolution, u32)> {
    [(FULL_HD, FULL_HD_KBPS), (ULTRA_HD, ULTRA_HD_KBPS)]
        .into_iter()
        .flat_map(|(resolution, bitrate_kbps)| {
            [Codec::Av1, Codec::Hevc, Codec::H264]
                .into_iter()
                .map(move |codec| (codec, resolution, bitrate_kbps))
        })
        .collect()
}

fn refresh_variants(codec: Codec, resolution: Resolution) -> Vec<Variant> {
    let block = if codec == Codec::H264 { 16 } else { 64 };
    let rows = resolution.height.div_ceil(block);
    let rows_per_frame = rows.div_ceil(FRAME_RATE);
    let former_sweep = rows.div_ceil(rows_per_frame);
    let named = |name: String, frames: Option<u32>| Variant {
        name,
        params: frames
            .map(|frames| ("intra-refresh-frames", frames.to_string()))
            .into_iter()
            .collect(),
    };

    vec![
        named(
            format!("sweep in {former_sweep} frames (former)"),
            Some(former_sweep),
        ),
        named(format!("sweep in {FRAME_RATE} frames (default)"), None),
        named("no intra refresh".to_owned(), Some(0)),
    ]
}

#[test]
#[ignore = "measures the intra refresh cadences on the VAAPI device"]
fn intra_refresh_cadence_times_and_scores() {
    for (codec, resolution, bitrate_kbps) in measured_cases() {
        compare(
            codec,
            resolution,
            bitrate_kbps,
            &refresh_variants(codec, resolution),
        );
    }
}
