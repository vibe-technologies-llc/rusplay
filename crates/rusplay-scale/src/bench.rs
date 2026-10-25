use std::time::{Duration, Instant};

use rusplay_core::{
    BitDepth, ChromaSubsampling, ColorRange, PixelFormat, Resolution, ScaleFilter, TransferFunction,
};

use crate::{
    ChromaSiting, ColorConverter, ConvertSettings, DEFAULT_ROW_ALIGNMENT, InputLight, PlaneLayout,
    ScalePipelines, ScaleSettings, Scaler, YuvMatrix,
    testing::{self, Gpu, Image},
};

const WARMUP_FRAMES: u32 = 300;
const BATCH_FRAMES: u32 = 100;
const BATCHES: usize = 7;

fn best_frame_time(gpu: &Gpu, mut encode_frame: impl FnMut(&mut wgpu::CommandEncoder)) -> Duration {
    let mut run = |frames: u32| {
        testing::submit(gpu, |encoder| {
            for _ in 0..frames {
                encode_frame(encoder);
            }
        });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device finishes the submitted work");
    };

    run(WARMUP_FRAMES);
    (0..BATCHES)
        .map(|_| {
            let start = Instant::now();
            run(BATCH_FRAMES);
            start.elapsed() / BATCH_FRAMES
        })
        .min()
        .unwrap_or_default()
}

fn scale_frame_time(gpu: &Gpu, settings: ScaleSettings) -> Duration {
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut scaler = Scaler::new(&pipelines, &gpu.device, settings).unwrap();
    let image = Image::pattern(settings.source.width, settings.source.height, 1);
    let source = testing::view(&testing::upload(gpu, &image));
    let target = testing::view(&testing::storage_target(
        gpu,
        settings.target.width,
        settings.target.height,
    ));

    best_frame_time(gpu, |encoder| {
        scaler.encode(&gpu.device, encoder, &source, &target);
    })
}

fn convert_frame_time(gpu: &Gpu, settings: ConvertSettings) -> Duration {
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut converter = ColorConverter::new(&pipelines, &gpu.device, settings).unwrap();
    let image = Image::pattern(settings.resolution.width, settings.resolution.height, 1);
    let source = testing::view(&testing::upload(gpu, &image));
    let frame = converter.create_frame(&gpu.device);

    best_frame_time(gpu, |encoder| {
        converter
            .encode(&gpu.device, encoder, &source, &frame)
            .unwrap();
    })
}

#[test]
#[ignore = "benchmark, run with --ignored --nocapture on a machine with a GPU"]
fn scaling_frame_times() {
    let Some(gpu) = testing::gpu() else {
        return;
    };

    let cases = [
        (ScaleFilter::Hermite, (3840, 2160), (1920, 1080)),
        (ScaleFilter::Hermite, (5120, 2880), (1920, 1080)),
        (ScaleFilter::Hermite, (7680, 4320), (1280, 720)),
        (ScaleFilter::Lanczos3, (1920, 1080), (3840, 2160)),
        (ScaleFilter::EwaLanczos, (1920, 1080), (3840, 2160)),
        (ScaleFilter::EwaRobidoux, (3840, 2160), (1920, 1080)),
    ];

    for (filter, source, target) in cases {
        let settings = ScaleSettings {
            filter,
            source: Resolution::new(source.0, source.1),
            target: Resolution::new(target.0, target.1),
            antiringing: 0.0,
            sigmoid: None,
        };

        println!(
            "{filter} {source:?} -> {target:?}: {:?}",
            scale_frame_time(gpu, settings)
        );
    }

    for (chroma, depth, layout, chroma_filter) in [
        (
            ChromaSubsampling::Yuv420,
            BitDepth::Eight,
            PlaneLayout::SemiPlanar,
            ScaleFilter::Hermite,
        ),
        (
            ChromaSubsampling::Yuv420,
            BitDepth::Ten,
            PlaneLayout::SemiPlanar,
            ScaleFilter::Hermite,
        ),
        (
            ChromaSubsampling::Yuv420,
            BitDepth::Ten,
            PlaneLayout::SemiPlanar,
            ScaleFilter::EwaLanczos,
        ),
        (
            ChromaSubsampling::Yuv444,
            BitDepth::Ten,
            PlaneLayout::Planar,
            ScaleFilter::Hermite,
        ),
    ] {
        let settings = ConvertSettings {
            resolution: Resolution::new(3840, 2160),
            format: PixelFormat { chroma, depth },
            layout,
            matrix: YuvMatrix::Bt709,
            range: ColorRange::Limited,
            transfer: TransferFunction::Srgb,
            input: InputLight::Linear,
            chroma_filter,
            chroma_siting: ChromaSiting::Left,
            row_alignment: DEFAULT_ROW_ALIGNMENT,
        };

        println!(
            "convert {chroma} {depth:?} {layout:?} {chroma_filter} -> 3840x2160: {:?}",
            convert_frame_time(gpu, settings)
        );
    }
}
