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
use rusplay_decode::{CpuFrame, DecodedBuffer};
use rusplay_scale::{
    ChromaSiting, FrameLayout, OutputLight, PlaneLayout, RgbConvertSettings, RgbConverter,
    ScalePipelines, ScaleSettings, Scaler, Sigmoid, YuvFrame,
};

use crate::{
    ClientError, FrameColor, FrameSink, Gpu, PresentedFrame, decode::DecodedVideo, slot::Slot,
    stats::Counters,
};

const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const OUTPUT_TEXTURES: usize = 4;
const UPLOAD_ROW_ALIGNMENT: u32 = 4;
const DECODED_PLANES: usize = 3;

pub(crate) type SharedTarget = Arc<Mutex<Option<Resolution>>>;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PresentOptions {
    pub scaling: ScalingConfig,
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
    upload: YuvFrame,
}

struct LinearSource {
    resolution: Resolution,
    view: wgpu::TextureView,
}

struct PresentStage {
    gpu: Gpu,
    options: PresentOptions,
    pipelines: ScalePipelines,
    unpacker: Option<Unpacker>,
    linear: Option<LinearSource>,
    scaler: Option<Scaler>,
    outputs: OutputPool,
}

impl PresentStage {
    fn new(gpu: Gpu, options: PresentOptions) -> Self {
        let pipelines = ScalePipelines::new(&gpu.device);

        Self {
            gpu,
            options,
            pipelines,
            unpacker: None,
            linear: None,
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
        let format = video.decoded.format;
        let cpu = match &video.decoded.buffer {
            DecodedBuffer::Cpu(cpu) => cpu,
            DecodedBuffer::DmaBuf(frame) => {
                return Err(ClientError::DmaBufPresentUnsupported {
                    fourcc: frame.image().fourcc,
                });
            }
        };

        let color = FrameColor::of_stream(video.color, format);
        let settings = convert_settings(&self.options, source, color);
        let device = &self.gpu.device;
        let unpacker = ensure_unpacker(&self.pipelines, device, &mut self.unpacker, settings)?;
        upload_planes(&self.gpu.queue, cpu, unpacker)?;
        drop(video.decoded);

        let target = links.target.lock().unwrap_or(source);
        let Some(output) = self.outputs.acquire(device, target) else {
            links.counters.frames_unpresented.increment();
            return Ok(());
        };
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rusplay-client-present"),
        });
        if target == source {
            unpacker
                .converter
                .encode(device, &mut encoder, &unpacker.upload, &output_view)?;
        } else {
            let linear = ensure_linear(device, &mut self.linear, source);
            unpacker
                .converter
                .encode(device, &mut encoder, &unpacker.upload, &linear.view)?;
            let settings = scale_settings(&self.options.scaling, source, target, color.light);
            ensure_scaler(&self.pipelines, device, &mut self.scaler, settings)?.encode(
                device,
                &mut encoder,
                &linear.view,
                &output_view,
            );
        }
        self.gpu.queue.submit([encoder.finish()]);

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

fn convert_settings(
    options: &PresentOptions,
    resolution: Resolution,
    color: FrameColor,
) -> RgbConvertSettings {
    RgbConvertSettings {
        resolution,
        format: color.coded,
        layout: PlaneLayout::Planar,
        matrix: color.matrix,
        range: color.space.range,
        transfer: color.space.transfer,
        output: color.light,
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
            unpacker.upload = unpacker.converter.create_frame(device);
            unpacker
        }
        None => {
            let converter = RgbConverter::new(pipelines, device, settings)?;
            let upload = converter.create_frame(device);
            Unpacker { converter, upload }
        }
    };

    Ok(slot.insert(unpacker))
}

fn ensure_linear<'a>(
    device: &wgpu::Device,
    slot: &'a mut Option<LinearSource>,
    resolution: Resolution,
) -> &'a LinearSource {
    let linear = slot
        .take()
        .filter(|linear| linear.resolution == resolution)
        .unwrap_or_else(|| LinearSource {
            resolution,
            view: create_texture(device, "rusplay-client-linear", resolution)
                .create_view(&wgpu::TextureViewDescriptor::default()),
        });

    slot.insert(linear)
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

fn scale_settings(
    scaling: &ScalingConfig,
    source: Resolution,
    target: Resolution,
    light: OutputLight,
) -> ScaleSettings {
    let upscaling = target.width > source.width || target.height > source.height;

    if upscaling {
        let upscale = &scaling.upscale;
        ScaleSettings {
            filter: upscale.filter,
            source,
            target,
            antiringing: upscale.antiringing,
            sigmoid: (upscale.sigmoid && light == OutputLight::Linear).then_some(Sigmoid {
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
    queue: &wgpu::Queue,
    cpu: &CpuFrame,
    unpacker: &Unpacker,
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
    let Some(mut staging) = queue.write_buffer_with(unpacker.upload.buffer(), 0, size) else {
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
