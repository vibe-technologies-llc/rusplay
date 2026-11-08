use std::{fs, io::Write, path::Path};

use super::*;

const FRAMES: usize = 3;
const TOLERANCE_DB: f64 = 0.001;
const FIXTURE_DIR: &str = "RUSPLAY_XPSNR_FIXTURE_DIR";

struct Case {
    name: &'static str,
    width: usize,
    height: usize,
    chroma: ChromaSubsampling,
    depth: BitDepth,
    fps: u32,
    expected: [f64; PLANES],
}

const fn case(
    name: &'static str,
    (width, height): (usize, usize),
    chroma: ChromaSubsampling,
    depth: BitDepth,
    fps: u32,
    expected: [f64; PLANES],
) -> Case {
    Case {
        name,
        width,
        height,
        chroma,
        depth,
        fps,
        expected,
    }
}

use BitDepth::{Eight, Ten, Twelve};
use ChromaSubsampling::{Yuv420, Yuv422, Yuv444};

const SMALL: (usize, usize) = (352, 288);
const ODD_SMALL: (usize, usize) = (321, 241);
const HD: (usize, usize) = (1280, 720);
const ODD_HD: (usize, usize) = (1281, 721);
const DOWNSAMPLED: (usize, usize) = (2560, 1440);
const NARROW_EDGE: (usize, usize) = (2530, 1460);
const TINY: (usize, usize) = (16, 16);
const NEAR_TINY: (usize, usize) = (64, 48);

const CASES: &[Case] = &[
    case(
        "small-420-8-first",
        SMALL,
        Yuv420,
        Eight,
        30,
        [34.303_249, 34.293_930, 34.287_399],
    ),
    case(
        "small-420-8-second",
        SMALL,
        Yuv420,
        Eight,
        60,
        [34.435_234, 34.360_165, 34.375_851],
    ),
    case(
        "small-422-8-first",
        SMALL,
        Yuv422,
        Eight,
        30,
        [34.370_502, 34.361_954, 34.333_504],
    ),
    case(
        "small-444-8-second",
        SMALL,
        Yuv444,
        Eight,
        60,
        [34.438_290, 34.406_242, 34.442_104],
    ),
    case(
        "small-420-10-second",
        SMALL,
        Yuv420,
        Ten,
        60,
        [35.306_393, 35.281_963, 35.374_798],
    ),
    case(
        "small-422-10-first",
        SMALL,
        Yuv422,
        Ten,
        30,
        [35.355_320, 35.351_185, 35.322_098],
    ),
    case(
        "small-444-10-second",
        SMALL,
        Yuv444,
        Ten,
        60,
        [35.360_523, 35.334_190, 35.325_279],
    ),
    case(
        "small-420-12-first",
        SMALL,
        Yuv420,
        Twelve,
        30,
        [35.592_236, 35.488_358, 35.523_823],
    ),
    case(
        "small-422-12-second",
        SMALL,
        Yuv422,
        Twelve,
        60,
        [35.519_619, 35.541_279, 35.519_371],
    ),
    case(
        "small-444-12-first",
        SMALL,
        Yuv444,
        Twelve,
        30,
        [35.645_859, 35.608_967, 35.642_044],
    ),
    case(
        "odd-small-420-8-second",
        ODD_SMALL,
        Yuv420,
        Eight,
        60,
        [33.504_459, 33.290_806, 33.296_661],
    ),
    case(
        "odd-small-422-10-first",
        ODD_SMALL,
        Yuv422,
        Ten,
        30,
        [33.739_452, 33.865_532, 33.694_260],
    ),
    case(
        "hd-420-8-second",
        HD,
        Yuv420,
        Eight,
        60,
        [40.567_348, 40.574_451, 40.583_866],
    ),
    case(
        "hd-422-10-first",
        HD,
        Yuv422,
        Ten,
        30,
        [41.366_570, 41.360_939, 41.353_561],
    ),
    case(
        "hd-444-12-second",
        HD,
        Yuv444,
        Twelve,
        60,
        [41.597_435, 41.606_064, 41.602_631],
    ),
    case(
        "odd-hd-420-10-second",
        ODD_HD,
        Yuv420,
        Ten,
        60,
        [41.480_164, 41.455_521, 41.464_207],
    ),
    case(
        "odd-hd-444-8-first",
        ODD_HD,
        Yuv444,
        Eight,
        30,
        [40.320_232, 40.328_236, 40.305_496],
    ),
    case(
        "downsampled-420-10-second",
        DOWNSAMPLED,
        Yuv420,
        Ten,
        60,
        [45.655_560, 45.644_974, 45.638_733],
    ),
    case(
        "downsampled-422-8-first",
        DOWNSAMPLED,
        Yuv422,
        Eight,
        30,
        [44.905_106, 44.893_604, 44.898_056],
    ),
    case(
        "downsampled-444-12-second",
        DOWNSAMPLED,
        Yuv444,
        Twelve,
        60,
        [45.911_663, 45.910_946, 45.908_112],
    ),
    case(
        "narrow-edge-420-8-second",
        NARROW_EDGE,
        Yuv420,
        Eight,
        60,
        [44.294_407, 44.288_311, 44.292_274],
    ),
    case(
        "narrow-edge-444-10-first",
        NARROW_EDGE,
        Yuv444,
        Ten,
        30,
        [45.818_542, 45.809_731, 45.813_110],
    ),
    case(
        "tiny-420-8-first",
        TINY,
        Yuv420,
        Eight,
        30,
        [43.297_829, 43.247_940, 43.762_222],
    ),
    case(
        "near-tiny-444-10-second",
        NEAR_TINY,
        Yuv444,
        Ten,
        60,
        [27.220_266, 27.233_372, 27.084_736],
    ),
];

struct Noise(u64);

impl Noise {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn seed(case: &Case) -> u64 {
    case.name.bytes().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
    })
}

fn texture(x: usize, y: usize, frame: usize, peak: u64, noise: &mut Noise) -> u16 {
    let region = (x / 48 + y / 40) % 4;
    let moved = x + 3 * frame;
    let gradient = (moved as u64 * peak / 4096 + y as u64 * peak / 8192) % (peak / 2);
    let detail = match region {
        0 => 0,
        1 => noise.next() % (peak / 64 + 1),
        2 => ((moved / 2 + y / 2) % 2) as u64 * (peak / 8),
        _ => noise.next() % (peak / 6 + 1),
    };

    u16::try_from((peak / 8 + gradient + detail).min(peak)).unwrap_or(u16::MAX)
}

fn generated_plane(width: usize, height: usize, frame: usize, peak: u64, seed: u64) -> SamplePlane {
    let mut noise = Noise::new(seed);
    let samples = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| texture(x, y, frame, peak, &mut noise))
        .collect();

    SamplePlane::new(width, height, samples).unwrap()
}

fn distorted(plane: &SamplePlane, peak: u64, seed: u64) -> SamplePlane {
    let mut noise = Noise::new(seed);
    let width = plane.width();
    let step = (peak / 255).max(1);
    let samples = plane
        .samples()
        .iter()
        .enumerate()
        .map(|(index, &value)| {
            let x = index % width;
            let amplitude = if x < width / 3 { 1 } else { 3 } * step;
            let offset = noise.next() % (2 * amplitude + 1);
            let shifted = (u64::from(value) + offset)
                .saturating_sub(amplitude)
                .min(peak);
            u16::try_from(shifted).unwrap_or(u16::MAX)
        })
        .collect();

    SamplePlane::new(width, plane.height(), samples).unwrap()
}

struct Sequence {
    format: PixelFormat,
    sources: Vec<SamplePicture>,
    reconstructions: Vec<SamplePicture>,
}

fn sequence(case: &Case) -> Sequence {
    let format = PixelFormat {
        chroma: case.chroma,
        depth: case.depth,
    };
    let peak = (1_u64 << case.depth.bits()) - 1;
    let (chroma_width, chroma_height) = chroma_size((case.width, case.height), case.chroma);
    let seed = seed(case);

    let sources: Vec<SamplePicture> = (0..FRAMES)
        .map(|frame| {
            let plane = |index: u64, width: usize, height: usize| {
                generated_plane(width, height, frame, peak, seed + index)
            };
            SamplePicture::new(
                format,
                [
                    plane(0, case.width, case.height),
                    plane(1, chroma_width, chroma_height),
                    plane(2, chroma_width, chroma_height),
                ],
            )
            .unwrap()
        })
        .collect();
    let reconstructions = sources
        .iter()
        .enumerate()
        .map(|(frame, source)| {
            let planes = source.planes();
            let base = seed ^ (0xD1B5 + frame as u64 * 7);
            SamplePicture::new(
                format,
                [
                    distorted(&planes[0], peak, base),
                    distorted(&planes[1], peak, base + 1),
                    distorted(&planes[2], peak, base + 2),
                ],
            )
            .unwrap()
        })
        .collect();

    Sequence {
        format,
        sources,
        reconstructions,
    }
}

fn score(case: &Case, sequence: &Sequence) -> QualityScore {
    let order = TemporalOrder::for_rate(FrameRate::new(case.fps, 1));
    let last = FRAMES - 1;
    let history = [
        sequence.sources[last - 1].luma(),
        sequence.sources[last - 2].luma(),
    ];

    measure(
        &sequence.sources[last],
        &sequence.reconstructions[last],
        &history,
        order,
    )
    .unwrap()
}

fn write_raw(path: &Path, pictures: &[SamplePicture], depth: BitDepth) {
    let mut file = fs::File::create(path).unwrap();
    for picture in pictures {
        for plane in picture.planes() {
            let bytes: Vec<u8> = plane
                .samples()
                .iter()
                .flat_map(|&sample| {
                    let little = sample.to_le_bytes();
                    if depth == Eight {
                        vec![little[0]]
                    } else {
                        little.to_vec()
                    }
                })
                .collect();
            file.write_all(&bytes).unwrap();
        }
    }
}

fn ffmpeg_format(format: PixelFormat) -> String {
    let chroma = match format.chroma {
        Yuv420 => "420",
        Yuv422 => "422",
        Yuv444 => "444",
    };
    let depth = match format.depth {
        Eight => "",
        Ten => "10le",
        Twelve => "12le",
    };

    format!("yuv{chroma}p{depth}")
}

#[test]
#[ignore = "writes the fixture sequences for ffmpeg; set RUSPLAY_XPSNR_FIXTURE_DIR"]
fn write_xpsnr_fixtures() {
    let directory = std::env::var_os(FIXTURE_DIR).expect("RUSPLAY_XPSNR_FIXTURE_DIR is set");
    let directory = Path::new(&directory);
    fs::create_dir_all(directory).unwrap();
    let mut index = fs::File::create(directory.join("cases.txt")).unwrap();

    for case in CASES {
        let sequence = sequence(case);
        write_raw(
            &directory.join(format!("{}.src.yuv", case.name)),
            &sequence.sources,
            case.depth,
        );
        write_raw(
            &directory.join(format!("{}.rec.yuv", case.name)),
            &sequence.reconstructions,
            case.depth,
        );
        writeln!(
            index,
            "{} {}x{} {} {}",
            case.name,
            case.width,
            case.height,
            ffmpeg_format(sequence.format),
            case.fps
        )
        .unwrap();
    }
}

fn agrees(ours: Decibels, expected: f64) -> bool {
    (f64::from(ours.value()) - expected).abs() <= TOLERANCE_DB
}

#[test]
fn xpsnr_matches_ffmpeg_on_the_reference_sequences() {
    let mismatches: Vec<String> = CASES
        .iter()
        .filter_map(|case| {
            let ours = score(case, &sequence(case)).xpsnr.as_array();
            let matched = ours
                .iter()
                .zip(case.expected)
                .all(|(&ours, expected)| agrees(ours, expected));
            (!matched).then(|| {
                format!(
                    "{}: ours {:?}, ffmpeg {:?}",
                    case.name,
                    ours.map(Decibels::value),
                    case.expected
                )
            })
        })
        .collect();

    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
