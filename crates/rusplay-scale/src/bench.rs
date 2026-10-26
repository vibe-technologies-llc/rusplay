use std::time::{Duration, Instant};

use rusplay_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, GamutMapMode, HdrLevels,
    MatrixCoefficients, PixelFormat, Resolution, ScaleFilter, TransferFunction,
};

use crate::{
    ChromaSiting, ColorChain, ColorChainSettings, ColorConverter, ColorLut, ColorLutSettings,
    ColorMapSettings, ColorMapper, ColorVolume, ConvertSettings, DEFAULT_ROW_ALIGNMENT,
    DynamicPeak, GamutMapping, InputLight, LineariseSettings, Lineariser, OutputLight, PlaneLayout,
    RgbConvertSettings, RgbConverter, ScalePipelines, ScaleSettings, Scaler, SourceChannels,
    ToneMapping, YuvFrame,
    testing::{self, Gpu, Image},
};

const WARMUP_FRAMES: u32 = 300;
const BATCH_FRAMES: u32 = 100;
const BATCHES: usize = 7;
const BAKES_PER_BATCH: usize = 8;
const LUT_SIZES: [u32; 3] = [65, 97, 129];

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

fn color_frame_time(gpu: &Gpu, settings: ColorMapSettings, size: (u32, u32)) -> Duration {
    let pipelines = ScalePipelines::new(&gpu.device);
    let mut mapper = ColorMapper::new(&pipelines, &gpu.device, settings).unwrap();
    let image = Image::pattern(size.0, size.1, 1);
    let source = testing::view(&testing::upload(gpu, &image));
    let target = testing::view(&testing::storage_target(gpu, size.0, size.1));

    best_frame_time(gpu, |encoder| {
        mapper.encode(&gpu.device, encoder, &source, &target);
    })
}

fn color_cases() -> Vec<(&'static str, ColorMapSettings)> {
    let sdr = |primaries| ColorVolume {
        primaries,
        ..ColorVolume::srgb_display()
    };
    let hdr = |primaries, peak_nits| ColorVolume {
        primaries,
        transfer: TransferFunction::Pq,
        peak_nits,
        ..ColorVolume::srgb_display()
    };
    let with_mode = |source, target, mode| ColorMapSettings {
        source,
        target,
        gamut: GamutMapping {
            mode,
            ..GamutMapping::default()
        },
        ..ColorMapSettings::default()
    };

    let tone_mapped = |dynamic_peak| ColorMapSettings {
        tone: ToneMapping {
            dynamic_peak,
            ..ToneMapping::default()
        },
        ..with_mode(
            hdr(ColorPrimaries::Bt2020, 4000.0),
            sdr(ColorPrimaries::DisplayP3),
            GamutMapMode::Perceptual,
        )
    };

    vec![
        (
            "bt2020 pq 4000 -> p3 sdr bt2390 perceptual",
            tone_mapped(None),
        ),
        (
            "bt2020 pq 4000 -> p3 sdr bt2390 dynamic perceptual",
            tone_mapped(Some(DynamicPeak::default())),
        ),
        (
            "bt2020 sdr -> p3 sdr perceptual",
            with_mode(
                sdr(ColorPrimaries::Bt2020),
                sdr(ColorPrimaries::DisplayP3),
                GamutMapMode::Perceptual,
            ),
        ),
        (
            "bt2020 pq -> p3 pq perceptual",
            with_mode(
                hdr(ColorPrimaries::Bt2020, 1000.0),
                hdr(ColorPrimaries::DisplayP3, 1000.0),
                GamutMapMode::Perceptual,
            ),
        ),
        (
            "bt2020 sdr -> p3 sdr relative",
            with_mode(
                sdr(ColorPrimaries::Bt2020),
                sdr(ColorPrimaries::DisplayP3),
                GamutMapMode::Relative,
            ),
        ),
    ]
}

fn hdr_frame(
    gpu: &Gpu,
    pipelines: &ScalePipelines,
    size: (u32, u32),
) -> (RgbConvertSettings, YuvFrame) {
    let resolution = Resolution::new(size.0, size.1);
    let format = PixelFormat {
        chroma: ChromaSubsampling::Yuv420,
        depth: BitDepth::Ten,
    };
    let encode = ConvertSettings {
        resolution,
        format,
        layout: PlaneLayout::Planar,
        matrix: MatrixCoefficients::Bt2020Ncl,
        range: ColorRange::Limited,
        transfer: TransferFunction::Pq,
        hdr: HdrLevels::default(),
        input: InputLight::Encoded,
        chroma_filter: ScaleFilter::Hermite,
        chroma_siting: ChromaSiting::Left,
        row_alignment: DEFAULT_ROW_ALIGNMENT,
    };
    let mut converter = ColorConverter::new(pipelines, &gpu.device, encode).unwrap();
    let frame = converter.create_frame(&gpu.device);
    let source = testing::view(&testing::upload(gpu, &Image::pattern(size.0, size.1, 1)));

    testing::submit(gpu, |encoder| {
        converter
            .encode(&gpu.device, encoder, &source, &frame)
            .unwrap();
    });

    let unpack = RgbConvertSettings {
        resolution,
        format,
        layout: PlaneLayout::Planar,
        matrix: MatrixCoefficients::Bt2020Ncl,
        range: ColorRange::Limited,
        transfer: TransferFunction::Pq,
        output: OutputLight::Encoded,
        chroma_filter: ScaleFilter::Lanczos3,
        chroma_siting: ChromaSiting::Left,
        row_alignment: DEFAULT_ROW_ALIGNMENT,
    };
    (unpack, frame)
}

fn present_color_cases() -> Vec<(&'static str, ColorMapSettings)> {
    let pq = ColorVolume {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        peak_nits: 4000.0,
        ..ColorVolume::srgb_display()
    };
    let p3 = ColorVolume {
        primaries: ColorPrimaries::DisplayP3,
        ..ColorVolume::srgb_display()
    };
    let p3_hdr = ColorVolume {
        primaries: ColorPrimaries::DisplayP3,
        transfer: TransferFunction::Pq,
        peak_nits: 1000.0,
        ..ColorVolume::srgb_display()
    };
    let with_mode = |target, mode| ColorMapSettings {
        source: pq,
        target,
        gamut: GamutMapping {
            mode,
            ..GamutMapping::default()
        },
        ..ColorMapSettings::default()
    };

    vec![
        (
            "bt2020 pq 4000 -> p3 sdr bt2390 perceptual",
            with_mode(p3, GamutMapMode::Perceptual),
        ),
        (
            "bt2020 pq 4000 -> p3 pq 1000 bt2390 perceptual",
            with_mode(p3_hdr, GamutMapMode::Perceptual),
        ),
        (
            "bt2020 pq 4000 -> p3 sdr bt2390 relative",
            with_mode(p3, GamutMapMode::Relative),
        ),
    ]
}

fn present_frame_times(gpu: &Gpu, size: (u32, u32)) {
    let pipelines = ScalePipelines::new(&gpu.device);
    let (unpack, frame) = hdr_frame(gpu, &pipelines, size);
    let mut converter = RgbConverter::new(&pipelines, &gpu.device, unpack).unwrap();
    let views = [(); 3].map(|()| testing::view(&testing::storage_target(gpu, size.0, size.1)));
    let linearise = LineariseSettings {
        channels: SourceChannels::RGBA,
        transfer: TransferFunction::Pq,
        hdr: HdrLevels::default(),
        matrix: MatrixCoefficients::Bt2020Ncl,
        output: OutputLight::Linear,
    };
    let mut lineariser = Lineariser::new(&pipelines, &gpu.device, linearise).unwrap();

    let unpack_only = best_frame_time(gpu, |encoder| {
        converter
            .encode(&gpu.device, encoder, &frame, &views[0])
            .unwrap();
    });
    println!("present unpack only {size:?}: {unpack_only:?}");

    for (name, settings) in present_color_cases() {
        let mut mapper = ColorMapper::new(&pipelines, &gpu.device, settings).unwrap();
        let analytic = best_frame_time(gpu, |encoder| {
            converter
                .encode(&gpu.device, encoder, &frame, &views[0])
                .unwrap();
            lineariser
                .encode(&gpu.device, encoder, &views[0], &views[1])
                .unwrap();
            mapper.encode(&gpu.device, encoder, &views[1], &views[2]);
        });
        println!("present {name} analytic {size:?}: {analytic:?}");

        let mut chain = ColorChain::new(
            &pipelines,
            &gpu.device,
            ColorChainSettings {
                transfer: TransferFunction::Pq,
                hdr: HdrLevels::default(),
                matrix: MatrixCoefficients::Bt2020Ncl,
                map: Some(settings),
            },
        )
        .unwrap();
        let fused = best_frame_time(gpu, |encoder| {
            converter
                .encode_with_chain(&gpu.device, encoder, &frame, &mut chain, &views[0])
                .unwrap();
        });
        println!("present {name} fused analytic {size:?}: {fused:?}");

        for lut_size in LUT_SIZES {
            let lut_settings = ColorLutSettings {
                transfer: TransferFunction::Pq,
                hdr: HdrLevels::default(),
                matrix: MatrixCoefficients::Bt2020Ncl,
                map: Some(settings),
                size: lut_size,
            };
            let mut lut = ColorLut::new(&pipelines, &gpu.device, lut_settings).unwrap();
            let fused = best_frame_time(gpu, |encoder| {
                converter
                    .encode_with_lut(&gpu.device, encoder, &frame, &mut lut, &views[0])
                    .unwrap();
            });
            let bake = bake_time(gpu, &pipelines, lut_settings);
            println!("present {name} lut {lut_size} {size:?}: {fused:?}, bake {bake:?}");
        }
    }
}

fn bake_time(gpu: &Gpu, pipelines: &ScalePipelines, settings: ColorLutSettings) -> Duration {
    let run = || {
        let mut luts: Vec<ColorLut> = (0..BAKES_PER_BATCH)
            .map(|_| ColorLut::new(pipelines, &gpu.device, settings).unwrap())
            .collect();
        let start = Instant::now();
        testing::submit(gpu, |encoder| {
            for lut in &mut luts {
                lut.encode_bake(encoder);
            }
        });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device finishes the submitted work");
        start.elapsed() / u32::try_from(BAKES_PER_BATCH).unwrap_or(1)
    };

    run();
    (0..BATCHES).map(|_| run()).min().unwrap_or_default()
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
            ChromaSubsampling::Yuv422,
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
            matrix: MatrixCoefficients::Bt709,
            range: ColorRange::Limited,
            transfer: TransferFunction::Srgb,
            hdr: HdrLevels::default(),
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

    for (name, settings) in color_cases() {
        println!(
            "color {name} 3840x2160: {:?}",
            color_frame_time(gpu, settings, (3840, 2160))
        );
    }

    present_frame_times(gpu, (3840, 2160));
}
