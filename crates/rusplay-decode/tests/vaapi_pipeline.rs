#![cfg(feature = "vaapi")]

mod common;
#[path = "../src/hardware.rs"]
mod hardware;

use std::{collections::VecDeque, env, path::PathBuf, sync::mpsc, thread, time::Duration};

use rusplay_config::VaapiDecoderConfig;
use rusplay_core::{BitDepth, Codec, EncodedFrame, Resolution, Timestamp};
use rusplay_decode::{DecodeError, DecodedBuffer, DecodedFrame, VaapiDecoder, VideoDecoder};
use rusplay_scale::{DmaBuf, DmaBufImporter, DmaBufPlane, WriteFence};

const CASES: [(&str, BitDepth); 2] = [
    ("hevc-rusplay-motion-8bit", BitDepth::Eight),
    ("hevc-rusplay-motion-10bit", BitDepth::Ten),
];
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const REPEATS: usize = 20;
const FRAMES_IN_FLIGHT: usize = 4;
const HANDOFF_DEPTH: usize = 2;
const FENCE_TIMEOUT: Duration = Duration::from_secs(2);
const REPORTED_MISMATCHES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Direct,
    Fenced,
    Held,
}

const MODES: [Mode; 3] = [Mode::Direct, Mode::Fenced, Mode::Held];

type Record = (usize, [u64; 3]);

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: wgpu::AdapterInfo,
}

fn gpu() -> Option<Gpu> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&rusplay_scale::device_descriptor(&adapter))
            .await
            .ok()?;

        device
            .features()
            .contains(rusplay_scale::DMABUF_FEATURES)
            .then(|| Gpu {
                device,
                queue,
                adapter: adapter.get_info(),
            })
    })
}

fn records(name: &str) -> Vec<Record> {
    let bytes = common::fixture(&format!("{name}.index"));

    std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (
                fields[0].parse().unwrap(),
                [1, 2, 3].map(|index| u64::from_str_radix(fields[index], 16).unwrap()),
            )
        })
        .collect()
}

fn packets(name: &str, records: &[Record]) -> Vec<EncodedFrame> {
    let encoded = common::fixture(&format!("{name}.hevc"));
    let mut offset = 0;

    records
        .iter()
        .enumerate()
        .map(|(index, (length, _))| {
            let data = encoded[offset..offset + length].to_vec().into();
            offset += length;
            EncodedFrame {
                codec: Codec::Hevc,
                timestamp: Timestamp::from_micros(u64::try_from(index).unwrap()),
                keyframe: index == 0,
                data,
            }
        })
        .collect()
}

fn hash(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

struct PlaneRead {
    buffer: wgpu::Buffer,
    row_bytes: usize,
}

struct Pending {
    index: usize,
    resolution: Resolution,
    depth: BitDepth,
    planes: [PlaneRead; 2],
    held: Option<DecodedFrame>,
}

fn owned_copy(device: &wgpu::Device, source: &wgpu::Texture) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pipeline-staged-plane"),
        size: source.size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: source.format(),
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn read_plane(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    source: &wgpu::Texture,
    staged: &wgpu::Texture,
) -> PlaneRead {
    let texel = source.format().block_copy_size(None).unwrap();
    let row_bytes = (source.width() * texel).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pipeline-plane-readback"),
        size: u64::from(row_bytes) * u64::from(source.height()),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    encoder.copy_texture_to_texture(
        source.as_image_copy(),
        staged.as_image_copy(),
        source.size(),
    );
    encoder.copy_texture_to_buffer(
        staged.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: None,
            },
        },
        source.size(),
    );

    PlaneRead {
        buffer,
        row_bytes: usize::try_from(row_bytes).unwrap(),
    }
}

fn map(gpu: &Gpu, plane: &PlaneRead) -> Vec<u8> {
    plane
        .buffer
        .map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();

    plane.buffer.get_mapped_range(..).unwrap().to_vec()
}

fn samples(bytes: &[u8], depth: BitDepth) -> Vec<u16> {
    if depth == BitDepth::Eight {
        bytes.iter().map(|&byte| u16::from(byte)).collect()
    } else {
        let (pairs, _) = bytes.as_chunks::<2>();
        pairs
            .iter()
            .map(|&pair| u16::from_le_bytes(pair) >> 6)
            .collect()
    }
}

fn encoded(values: impl Iterator<Item = u16>, depth: BitDepth) -> u64 {
    if depth == BitDepth::Eight {
        hash(values.map(|value| value.to_le_bytes()[0]))
    } else {
        hash(values.flat_map(u16::to_le_bytes))
    }
}

fn checksums(gpu: &Gpu, pending: &Pending) -> [u64; 3] {
    let sample_bytes = if pending.depth == BitDepth::Eight {
        1
    } else {
        2
    };
    let width = usize::try_from(pending.resolution.width).unwrap();
    let height = usize::try_from(pending.resolution.height).unwrap();
    let rows = |plane: &PlaneRead, row_bytes: usize, count: usize| -> Vec<u16> {
        map(gpu, plane)
            .chunks_exact(plane.row_bytes)
            .take(count)
            .flat_map(|row| samples(&row[..row_bytes], pending.depth))
            .collect()
    };

    let luma = rows(&pending.planes[0], width * sample_bytes, height);
    let chroma = rows(&pending.planes[1], width * sample_bytes, height / 2);

    [
        encoded(luma.iter().copied(), pending.depth),
        encoded(chroma.iter().step_by(2).copied(), pending.depth),
        encoded(chroma.iter().skip(1).step_by(2).copied(), pending.depth),
    ]
}

fn decoder_thread(
    config: &VaapiDecoderConfig,
    packets: &[EncodedFrame],
    output: &mpsc::SyncSender<(usize, DecodedFrame)>,
) -> Option<DecodeError> {
    let mut decoder = match VaapiDecoder::new(Codec::Hevc, config) {
        Ok(decoder) => decoder,
        Err(error) => return Some(error),
    };

    for _ in 0..REPEATS {
        for (index, packet) in packets.iter().enumerate() {
            match decoder.decode(packet) {
                Ok(Some(frame)) => {
                    if output.send((index, frame)).is_err() {
                        return None;
                    }
                }
                Ok(None) => {}
                Err(error) => return Some(error),
            }
        }
    }

    None
}

fn describe(frame: &DecodedFrame) {
    let DecodedBuffer::DmaBuf(buffer) = &frame.buffer else {
        panic!("expected DMA-BUF")
    };
    let image = buffer.image();
    let planes: Vec<_> = image
        .planes
        .iter()
        .map(|plane| (plane.object, plane.offset, plane.pitch))
        .collect();

    eprintln!(
        "surface: fourcc {:?}, modifier {:#x}, {}x{}, {} objects, planes (object, offset, pitch) {planes:?}",
        image.fourcc,
        image.modifier,
        image.width,
        image.height,
        image.objects.len(),
    );
}

fn read_frame(
    gpu: &Gpu,
    importer: &mut DmaBufImporter,
    staged: &mut Option<[wgpu::Texture; 2]>,
    frame: &DecodedFrame,
    mode: Mode,
) -> ([PlaneRead; 2], usize) {
    let DecodedBuffer::DmaBuf(buffer) = &frame.buffer else {
        panic!("expected DMA-BUF")
    };
    let image = buffer.image();
    let planes: Vec<_> = image
        .planes
        .iter()
        .enumerate()
        .map(|(plane, layout)| DmaBufPlane {
            fd: image.plane_fd(plane).unwrap(),
            offset: layout.offset,
            stride: layout.pitch,
        })
        .collect();
    let mut late_fences = 0;
    if mode == Mode::Fenced {
        for plane in &planes {
            let fence = rusplay_scale::wait_for_writes(plane.fd, Duration::ZERO).unwrap();
            if fence == WriteFence::TimedOut {
                late_fences += 1;
                rusplay_scale::wait_for_writes(plane.fd, FENCE_TIMEOUT).unwrap();
            }
        }
    }
    let surface = importer
        .import_external(&DmaBuf {
            fourcc: image.fourcc.as_u32(),
            modifier: image.modifier,
            width: image.width,
            height: image.height,
            planes,
        })
        .unwrap();
    let luma = surface.texture(0).unwrap();
    let chroma = surface.texture(1).unwrap();
    let owned = staged.get_or_insert_with(|| {
        [
            owned_copy(&gpu.device, luma),
            owned_copy(&gpu.device, chroma),
        ]
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let acquire = surface.acquire(&gpu.device).unwrap();
    let reads = [
        read_plane(gpu, &mut encoder, luma, &owned[0]),
        read_plane(gpu, &mut encoder, chroma, &owned[1]),
    ];
    let release = surface.release(&gpu.device, &mut encoder).unwrap();
    gpu.queue
        .submit(acquire.into_iter().chain([encoder.finish()]).chain(release));

    (reads, late_fences)
}

fn run(
    gpu: &Gpu,
    importer: &mut DmaBufImporter,
    config: &VaapiDecoderConfig,
    (name, depth): (&str, BitDepth),
    mode: Mode,
) -> (usize, usize, usize) {
    let records = records(name);
    let packets = packets(name, &records);
    let (sender, receiver) = mpsc::sync_channel(HANDOFF_DEPTH);
    let thread_config = config.clone();
    let worker = thread::spawn(move || decoder_thread(&thread_config, &packets, &sender));

    let mut in_flight: VecDeque<Pending> = VecDeque::new();
    let mut staged: Option<[wgpu::Texture; 2]> = None;
    let mut compared = 0_usize;
    let mut wrong = 0_usize;
    let mut late_fences = 0_usize;
    let mut described = false;
    let mut check = |pending: Pending| {
        let actual = checksums(gpu, &pending);
        let expected = records[pending.index].1;
        compared += 1;
        if actual != expected {
            wrong += 1;
            if wrong <= REPORTED_MISMATCHES {
                let planes: Vec<_> = (0..3).filter(|&p| actual[p] != expected[p]).collect();
                eprintln!(
                    "{name} {mode:?} output {compared} frame {}: planes {planes:?} differ",
                    pending.index
                );
            }
        }
        drop(pending.held);
    };

    for (index, frame) in receiver {
        if !described && mode == Mode::Direct {
            describe(&frame);
            described = true;
        }
        let (reads, late) = read_frame(gpu, importer, &mut staged, &frame, mode);
        late_fences += late;

        let resolution = frame.resolution;
        let held = if mode == Mode::Held {
            Some(frame)
        } else {
            rusplay_scale::release_after_submission(&gpu.queue, frame);
            None
        };
        in_flight.push_back(Pending {
            index,
            resolution,
            depth,
            planes: reads,
            held,
        });

        if in_flight.len() > FRAMES_IN_FLIGHT {
            check(in_flight.pop_front().unwrap());
        }
    }
    while let Some(pending) = in_flight.pop_front() {
        check(pending);
    }

    if let Some(error) = worker.join().unwrap() {
        panic!("{name}: {error}");
    }
    assert_eq!(compared, records.len() * REPEATS, "{name}");

    (compared, wrong, late_fences)
}

#[test]
#[ignore = "takes minutes; run on a client that shows corruption"]
fn pipelined_hevc_import_matches_software_reference() {
    let mut config = VaapiDecoderConfig::default();
    if let Some(device) = env::var_os("RUSPLAY_VAAPI_DEVICE") {
        config.device = PathBuf::from(device);
    }
    if !hardware::device_present(&config.device) {
        return;
    }
    let Some(gpu) = gpu() else {
        hardware::missing("a DMA-BUF capable Vulkan adapter");
        return;
    };
    let mut importer = DmaBufImporter::new(&gpu.device).unwrap();
    eprintln!(
        "vaapi device {}, vulkan adapter {} ({}, {})",
        config.device.display(),
        gpu.adapter.name,
        gpu.adapter.driver,
        gpu.adapter.driver_info
    );

    let mut failures = 0;
    for case in CASES {
        for mode in MODES {
            let (compared, wrong, late_fences) = run(&gpu, &mut importer, &config, case, mode);
            eprintln!(
                "{} {mode:?}: {compared} frames, {wrong} wrong, {late_fences} fences still busy",
                case.0
            );
            failures += wrong;
        }
    }

    assert_eq!(failures, 0);
}
