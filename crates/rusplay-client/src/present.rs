mod mapping;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use parking_lot::Mutex;
use rusplay_config::ScalingConfig;
use rusplay_core::Resolution;
use rusplay_decode::{CpuFrame, DecodedBuffer, DmaBufImage};
use rusplay_scale::{
    ChromaSiting, ColorVolume, DmaBuf, DmaBufImporter, DmaBufPlane, DmaBufTexture, FrameLayout,
    OutputLight, PlaneLayout, RgbConvertSettings, RgbConverter, ScalePipelines, ScaleSettings,
    Scaler, Sigmoid, YuvFrame,
};

#[cfg(test)]
pub(crate) use crate::present::mapping::Processing;
pub(crate) use crate::present::mapping::{ColorStage, MappingOptions, whole_nits};
use crate::{
    ClientError, FrameColor, FrameSink, Gpu, PresentedFrame,
    decode::DecodedVideo,
    present::mapping::{ColorRoute, FusedColor},
    slot::Slot,
    stats::Counters,
};

const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const OUTPUT_TEXTURES: usize = 4;
const UPLOAD_ROW_ALIGNMENT: u32 = 4;
const DECODED_PLANES: usize = 3;
const SCRATCH_TEXTURES: usize = 2;

pub(crate) type SharedTarget = Arc<Mutex<Option<Resolution>>>;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PresentOptions {
    pub scaling: ScalingConfig,
    pub mapping: MappingOptions,
    pub display: ColorVolume,
}

pub(crate) struct PresentLinks {
    pub input: Arc<Slot<DecodedVideo>>,
    pub target: SharedTarget,
    pub sink: Box<dyn FrameSink>,
    pub stop: Arc<AtomicBool>,
    pub counters: Arc<Counters>,
}

pub(crate) fn run(
    gpu: Gpu,
    options: PresentOptions,
    mut links: PresentLinks,
) -> Result<(), ClientError> {
    let mut stage = PresentStage::new(gpu, options);

    let result = loop {
        let Some(video) = links.input.take() else {
            break Ok(());
        };
        if let Err(error) = stage.present(video, &mut links) {
            break Err(error);
        }
    };

    links.stop.store(true, Ordering::Relaxed);
    drop(links.input.close());
    result
}

struct Unpacker {
    converter: RgbConverter,
    upload: Option<YuvFrame>,
}

enum UnpackInput {
    Upload,
    Planes(DmaBufTexture),
}

#[derive(Default)]
struct Scratch {
    resolution: Option<Resolution>,
    views: Vec<wgpu::TextureView>,
}

impl Scratch {
    fn ensure(
        &mut self,
        device: &wgpu::Device,
        resolution: Resolution,
        count: usize,
    ) -> &[wgpu::TextureView] {
        if self.resolution != Some(resolution) {
            self.resolution = Some(resolution);
            self.views.clear();
        }
        while self.views.len() < count {
            self.views.push(
                create_texture(device, "rusplay-client-scratch", resolution)
                    .create_view(&wgpu::TextureViewDescriptor::default()),
            );
        }

        &self.views
    }
}

struct PresentStage {
    gpu: Gpu,
    options: PresentOptions,
    pipelines: ScalePipelines,
    unpacker: Option<Unpacker>,
    importer: Option<DmaBufImporter>,
    scratch: Scratch,
    color: ColorStage,
    scaler: Option<Scaler>,
    outputs: OutputPool,
}

impl PresentStage {
    fn new(gpu: Gpu, options: PresentOptions) -> Self {
        let pipelines = ScalePipelines::new(&gpu.device);
        let color = ColorStage::new(&pipelines, options.mapping);

        Self {
            gpu,
            options,
            pipelines,
            unpacker: None,
            importer: None,
            scratch: Scratch::default(),
            color,
            scaler: None,
            outputs: OutputPool::default(),
        }
    }

    fn present(
        &mut self,
        video: DecodedVideo,
        links: &mut PresentLinks,
    ) -> Result<(), ClientError> {
        let source = video.decoded.resolution;
        let decoded = FrameColor::of_stream(video.color, video.decoded.format);
        let display = links.sink.color_target().unwrap_or(self.options.display);
        let device = &self.gpu.device;
        let route = self.color.route(device, decoded, display)?;

        let (layout, input) = match &video.decoded.buffer {
            DecodedBuffer::Cpu(_) => (PlaneLayout::Planar, UnpackInput::Upload),
            DecodedBuffer::DmaBuf(frame) => {
                let importer = match &mut self.importer {
                    Some(importer) => importer,
                    None => self.importer.insert(DmaBufImporter::new(device)?),
                };
                let planes = importer.import(&describe(frame.image())?)?;
                (PlaneLayout::SemiPlanar, UnpackInput::Planes(planes))
            }
        };
        let settings = convert_settings(&self.options, source, decoded, route.unpack, layout);
        let unpacker = ensure_unpacker(&self.pipelines, device, &mut self.unpacker, settings)?;

        let lease = match &video.decoded.buffer {
            DecodedBuffer::Cpu(cpu) => {
                upload_planes(device, &self.gpu.queue, cpu, unpacker)?;
                drop(video.decoded);
                None
            }
            DecodedBuffer::DmaBuf(_) => Some(video.decoded),
        };

        let target = links.target.lock().unwrap_or(source);
        let Some(output) = self.outputs.acquire(device, target) else {
            links.counters.frames_unpresented.increment();
            return Ok(());
        };
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let scale = (target != source)
            .then(|| scale_settings(&self.options.scaling, source, target, !display.is_hdr()));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rusplay-client-present"),
        });
        let passes = 1 + route.passes() + usize::from(scale.is_some());
        let scratch = self
            .scratch
            .ensure(device, source, (passes - 1).min(SCRATCH_TEXTURES));
        let chain = Chain {
            scratch,
            output: &output_view,
            passes,
        };
        let mut pass = 0;

        unpack(
            unpacker,
            device,
            &mut encoder,
            &input,
            self.color.fused(&route),
            chain.destination(pass),
        )?;
        pass += 1;
        encode_color(
            &mut self.color,
            device,
            &mut encoder,
            &route,
            &chain,
            &mut pass,
        )?;
        if let Some(settings) = scale {
            ensure_scaler(&self.pipelines, device, &mut self.scaler, settings)?.encode(
                device,
                &mut encoder,
                chain.source(pass),
                &output_view,
            );
        }
        self.gpu.queue.submit([encoder.finish()]);
        if let Some(lease) = lease {
            rusplay_scale::release_after_submission(&self.gpu.queue, lease);
        }
        let color = route.output;

        links.counters.frames_presented.increment();
        links.sink.present(PresentedFrame {
            frame: video.number,
            keyframe: video.keyframe,
            source,
            color,
            received_at: video.received_at,
            decoded_at: video.decoded_at,
            submitted_at: Instant::now(),
            texture: output,
        });

        Ok(())
    }
}

struct Chain<'a> {
    scratch: &'a [wgpu::TextureView],
    output: &'a wgpu::TextureView,
    passes: usize,
}

impl Chain<'_> {
    fn destination(&self, pass: usize) -> &wgpu::TextureView {
        if pass + 1 == self.passes {
            self.output
        } else {
            &self.scratch[pass % SCRATCH_TEXTURES]
        }
    }

    fn source(&self, pass: usize) -> &wgpu::TextureView {
        &self.scratch[(pass - 1) % SCRATCH_TEXTURES]
    }
}

fn encode_color(
    color: &mut ColorStage,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    route: &ColorRoute,
    chain: &Chain<'_>,
    pass: &mut usize,
) -> Result<(), ClientError> {
    if route.linearise.is_some() {
        color.linearise(
            device,
            encoder,
            chain.source(*pass),
            chain.destination(*pass),
        )?;
        *pass += 1;
    }
    if route.map.is_some() {
        color.map(
            device,
            encoder,
            chain.source(*pass),
            chain.destination(*pass),
        );
        *pass += 1;
    }

    Ok(())
}

fn unpack(
    unpacker: &mut Unpacker,
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    input: &UnpackInput,
    fused: Option<FusedColor<'_>>,
    target: &wgpu::TextureView,
) -> Result<(), ClientError> {
    let converter = &mut unpacker.converter;

    match (input, &unpacker.upload, fused) {
        (UnpackInput::Upload, None, _) => Ok(()),
        (UnpackInput::Upload, Some(upload), None) => {
            converter.encode(device, encoder, upload, target)
        }
        (UnpackInput::Upload, Some(upload), Some(FusedColor::Chain(chain))) => {
            converter.encode_with_chain(device, encoder, upload, chain, target)
        }
        (UnpackInput::Upload, Some(upload), Some(FusedColor::Lut(lut))) => {
            converter.encode_with_lut(device, encoder, upload, lut, target)
        }
        (UnpackInput::Planes(planes), _, None) => {
            converter.encode_planes(device, encoder, planes, target)
        }
        (UnpackInput::Planes(planes), _, Some(FusedColor::Chain(chain))) => {
            converter.encode_planes_with_chain(device, encoder, planes, chain, target)
        }
        (UnpackInput::Planes(planes), _, Some(FusedColor::Lut(lut))) => {
            converter.encode_planes_with_lut(device, encoder, planes, lut, target)
        }
    }
    .map_err(ClientError::from)
}

fn describe(image: &DmaBufImage) -> Result<DmaBuf<'_>, ClientError> {
    let planes = image
        .planes
        .iter()
        .enumerate()
        .map(|(index, plane)| {
            let fd = image
                .plane_fd(index)
                .ok_or(ClientError::DmaBufObjectMissing {
                    plane: index,
                    object: plane.object,
                })?;
            Ok(DmaBufPlane {
                fd,
                offset: plane.offset,
                stride: plane.pitch,
            })
        })
        .collect::<Result<Vec<_>, ClientError>>()?;

    Ok(DmaBuf {
        fourcc: image.fourcc.as_u32(),
        modifier: image.modifier,
        width: image.width,
        height: image.height,
        planes,
    })
}

fn convert_settings(
    options: &PresentOptions,
    resolution: Resolution,
    color: FrameColor,
    output: OutputLight,
    layout: PlaneLayout,
) -> RgbConvertSettings {
    RgbConvertSettings {
        resolution,
        format: color.coded,
        layout,
        matrix: color.matrix,
        range: color.space.range,
        transfer: color.space.transfer,
        output,
        chroma_filter: options.scaling.upscale.chroma_filter,
        chroma_siting: ChromaSiting::Left,
        row_alignment: UPLOAD_ROW_ALIGNMENT,
    }
}

fn ensure_unpacker<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<Unpacker>,
    settings: RgbConvertSettings,
) -> Result<&'a mut Unpacker, ClientError> {
    let unpacker = match slot.take() {
        Some(unpacker) if unpacker.converter.settings() == settings => unpacker,
        Some(mut unpacker) => {
            unpacker.converter.configure(device, settings)?;
            unpacker.upload = None;
            unpacker
        }
        None => Unpacker {
            converter: RgbConverter::new(pipelines, device, settings)?,
            upload: None,
        },
    };

    Ok(slot.insert(unpacker))
}

fn ensure_scaler<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<Scaler>,
    settings: ScaleSettings,
) -> Result<&'a mut Scaler, ClientError> {
    let scaler = match slot.take() {
        Some(mut scaler) => {
            scaler.configure(device, settings)?;
            scaler
        }
        None => Scaler::new(pipelines, device, settings)?,
    };

    Ok(slot.insert(scaler))
}

pub(crate) fn scale_settings(
    scaling: &ScalingConfig,
    source: Resolution,
    target: Resolution,
    sigmoid: bool,
) -> ScaleSettings {
    let upscaling = target.width > source.width || target.height > source.height;

    if upscaling {
        let upscale = &scaling.upscale;
        ScaleSettings {
            filter: upscale.filter,
            source,
            target,
            antiringing: upscale.antiringing,
            sigmoid: (upscale.sigmoid && sigmoid).then_some(Sigmoid {
                center: upscale.sigmoid_center,
                slope: upscale.sigmoid_slope,
            }),
        }
    } else {
        ScaleSettings {
            filter: scaling.downscale.filter,
            source,
            target,
            antiringing: 0.0,
            sigmoid: None,
        }
    }
}

fn upload_planes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cpu: &CpuFrame,
    unpacker: &mut Unpacker,
) -> Result<(), ClientError> {
    let layout = unpacker.converter.layout();
    let found = cpu.plane_count();
    if found != DECODED_PLANES || layout.planes.len() != DECODED_PLANES {
        return Err(ClientError::PlaneCountMismatch {
            found,
            expected: layout.planes.len(),
        });
    }
    check_planes(cpu, layout)?;

    let Some(size) = wgpu::BufferSize::new(layout.size) else {
        return Ok(());
    };
    let upload = unpacker
        .upload
        .get_or_insert_with(|| unpacker.converter.create_frame(device));
    let Some(mut staging) = queue.write_buffer_with(upload.buffer(), 0, size) else {
        return Ok(());
    };

    for (plane, expected) in cpu.planes().zip(&layout.planes) {
        let row_bytes = expected.width as usize * usize::from(plane.bytes_per_sample);
        let offset = usize::try_from(expected.offset).unwrap_or(usize::MAX);
        let stride = expected.stride as usize;

        for row in 0..expected.height as usize {
            let source = &plane.data[row * plane.stride..row * plane.stride + row_bytes];
            let start = offset + row * stride;
            staging
                .slice(start..start + row_bytes)
                .copy_from_slice(source);
        }
    }

    Ok(())
}

fn check_planes(cpu: &CpuFrame, layout: &FrameLayout) -> Result<(), ClientError> {
    for (index, (plane, expected)) in cpu.planes().zip(&layout.planes).enumerate() {
        if (plane.width, plane.height) != (expected.width, expected.height)
            || u32::from(plane.bytes_per_sample) != layout.bytes_per_sample
        {
            return Err(ClientError::PlaneShapeMismatch {
                plane: index,
                found_width: plane.width,
                found_height: plane.height,
                expected_width: expected.width,
                expected_height: expected.height,
            });
        }

        let row_bytes = plane.width as usize * usize::from(plane.bytes_per_sample);
        let needed = (plane.height as usize).saturating_sub(1) * plane.stride + row_bytes;
        if plane.data.len() < needed {
            return Err(ClientError::PlaneTooShort {
                plane: index,
                found: plane.data.len(),
                needed,
            });
        }
    }

    Ok(())
}

#[derive(Default)]
struct OutputPool {
    resolution: Option<Resolution>,
    textures: Vec<Arc<wgpu::Texture>>,
}

impl OutputPool {
    fn acquire(
        &mut self,
        device: &wgpu::Device,
        resolution: Resolution,
    ) -> Option<Arc<wgpu::Texture>> {
        if self.resolution != Some(resolution) {
            self.resolution = Some(resolution);
            self.textures.clear();
        }

        if let Some(free) = self
            .textures
            .iter()
            .find(|texture| Arc::strong_count(texture) == 1)
        {
            return Some(Arc::clone(free));
        }

        if self.textures.len() < OUTPUT_TEXTURES {
            let texture = Arc::new(create_texture(device, "rusplay-client-output", resolution));
            self.textures.push(Arc::clone(&texture));
            return Some(texture);
        }

        None
    }
}

fn create_texture(device: &wgpu::Device, label: &str, resolution: Resolution) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: resolution.width,
            height: resolution.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OUTPUT_FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[cfg(all(test, feature = "software", feature = "vaapi"))]
mod tests {
    use std::{fs, path::PathBuf};

    use rusplay_config::{Config, SoftwareDecoderConfig, VaapiDecoderConfig, VideoDecoderConfig};
    use rusplay_core::{Codec, EncodedFrame, Timestamp};

    use super::*;
    use crate::stats::Counters;

    const IVF_HEADER_LEN: usize = 32;
    const IVF_FRAME_HEADER_LEN: usize = 12;
    const RGBA16F_BYTES: u32 = 8;

    fn gpu() -> Option<Gpu> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..wgpu::RequestAdapterOptions::default()
        }))
        .ok()?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&rusplay_scale::device_descriptor(&adapter)))
                .ok()?;

        device
            .features()
            .contains(rusplay_scale::DMABUF_FEATURES)
            .then_some(Gpu { device, queue })
    }

    fn ivf_frames(name: &str) -> Vec<Vec<u8>> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../rusplay-decode/tests/data")
            .join(name);
        let file = fs::read(path).unwrap();

        let mut frames = Vec::new();
        let mut cursor = IVF_HEADER_LEN;
        while cursor + IVF_FRAME_HEADER_LEN <= file.len() {
            let size = u32::from_le_bytes(file[cursor..cursor + 4].try_into().unwrap());
            let start = cursor + IVF_FRAME_HEADER_LEN;
            let end = start + usize::try_from(size).unwrap();
            frames.push(file[start..end].to_vec());
            cursor = end;
        }
        frames
    }

    fn read_texture(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u8> {
        let (width, height) = (texture.width(), texture.height());
        let tight_row = (width * RGBA16F_BYTES) as usize;
        let row_bytes =
            (width * RGBA16F_BYTES).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rusplay-test-readback"),
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
            texture.size(),
        );
        gpu.queue.submit([encoder.finish()]);
        staging.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();

        let bytes = staging
            .get_mapped_range(..)
            .unwrap()
            .chunks_exact(row_bytes as usize)
            .flat_map(|row| &row[..tight_row])
            .copied()
            .collect();
        staging.unmap();
        bytes
    }

    fn present_clip(
        gpu: &Gpu,
        backend: &VideoDecoderConfig,
        clip: &str,
        target: Option<Resolution>,
    ) -> Option<Vec<Vec<u8>>> {
        let mut decoder = rusplay_decode::open(Codec::Av1, backend).ok()?;
        let config = Config::default();
        let presented = Arc::new(Mutex::new(Vec::new()));
        let sink = {
            let presented = Arc::clone(&presented);
            move |frame: PresentedFrame| presented.lock().push(frame)
        };
        let mut links = PresentLinks {
            input: Arc::new(Slot::new()),
            target: Arc::new(Mutex::new(target)),
            sink: Box::new(sink),
            stop: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(Counters::default()),
        };
        let mut stage = PresentStage::new(
            gpu.clone(),
            PresentOptions {
                scaling: config.scaling,
                mapping: MappingOptions::from(&config.color),
                display: ColorVolume::srgb_display(),
            },
        );

        for (number, data) in (0..).zip(ivf_frames(clip)) {
            let frame = EncodedFrame {
                codec: Codec::Av1,
                timestamp: Timestamp::ZERO,
                keyframe: number == 0,
                data,
            };
            let Some(picture) = decoder.decode(&frame).unwrap() else {
                continue;
            };
            let video = DecodedVideo {
                decoded: picture,
                number,
                keyframe: frame.keyframe,
                color: config.video.color,
                received_at: Instant::now(),
                decoded_at: Instant::now(),
            };
            stage.present(video, &mut links).unwrap();
        }

        let frames = presented.lock();
        Some(
            frames
                .iter()
                .map(|frame| read_texture(gpu, &frame.texture))
                .collect(),
        )
    }

    #[test]
    fn vaapi_dmabuf_frames_present_like_software_frames() {
        let Some(gpu) = gpu() else {
            return;
        };
        let vaapi = VideoDecoderConfig::Vaapi(VaapiDecoderConfig::default());
        let software = VideoDecoderConfig::Software(SoftwareDecoderConfig::default());

        for clip in ["av1-420-8bit.ivf", "av1-420-10bit.ivf"] {
            for target in [None, Some(Resolution::new(400, 224))] {
                let Some(hardware) = present_clip(&gpu, &vaapi, clip, target) else {
                    return;
                };
                let reference = present_clip(&gpu, &software, clip, target).unwrap();

                assert!(!hardware.is_empty(), "{clip}");
                assert_eq!(hardware.len(), reference.len(), "{clip}");
                for (index, (actual, expected)) in hardware.iter().zip(&reference).enumerate() {
                    assert!(
                        actual == expected,
                        "{clip} {target:?} frame {index} differs"
                    );
                }
            }
        }
    }
}
