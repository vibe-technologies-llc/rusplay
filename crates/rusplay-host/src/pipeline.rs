use std::sync::mpsc;

use rusplay_capture::{Frame, FrameBuffer};
use rusplay_config::Config;
use rusplay_core::{
    Codec, ColorPrimaries, ColorSpace, EncodedFrame, PixelFormat, Resolution, Timestamp,
};
use rusplay_encode::{VideoEncoder, input::fourcc_for};
use rusplay_protocol::StreamDescription;
use rusplay_scale::{
    ChromaSiting, ColorConverter, ColorMapSettings, ColorMapper, ColorVolume, ConvertSettings,
    GamutMapping, InputLight, PlaneLayout, ScalePipelines, ScaleSettings, Scaler, Sigmoid,
    ToneMapping, YuvFrame,
};

use crate::{
    Gpu, HostError,
    upload::{EncoderPlanes, check_shm_frame, texture_format_for},
};

const CONVERT_ROW_ALIGNMENT: u32 = 4;
const LINEAR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamFormat {
    pub codec: Codec,
    pub resolution: Resolution,
    pub format: PixelFormat,
    pub color: ColorSpace,
}

impl StreamFormat {
    pub(crate) const fn describe(self, generation: u32) -> StreamDescription {
        StreamDescription {
            generation,
            codec: self.codec,
            resolution: self.resolution,
            format: self.format,
            color: self.color,
        }
    }
}

pub(crate) trait VideoPipeline {
    fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Option<EncodedFrame>, HostError>;

    fn repeat(
        &mut self,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, HostError>;

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), HostError>;

    fn stream_format(&self) -> Option<StreamFormat>;
}

struct Upload {
    resolution: Resolution,
    format: wgpu::TextureFormat,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct Linear {
    resolution: Resolution,
    view: wgpu::TextureView,
}

struct Conversion {
    converter: ColorConverter,
    output: YuvFrame,
    readback: wgpu::Buffer,
    planes: EncoderPlanes,
}

struct OpenEncoder {
    resolution: Resolution,
    encoder: Box<dyn VideoEncoder>,
}

pub(crate) struct GpuPipeline {
    gpu: Gpu,
    config: Config,
    pipelines: ScalePipelines,
    upload: Option<Upload>,
    scaled: Option<Linear>,
    mapped: Option<Linear>,
    scaler: Option<Scaler>,
    mapper: Option<ColorMapper>,
    conversion: Option<Conversion>,
    encoder: Option<OpenEncoder>,
    input: Option<Frame>,
    bitrate_kbps: Option<u32>,
}

impl GpuPipeline {
    pub(crate) fn new(gpu: Gpu, config: Config) -> Self {
        let pipelines = ScalePipelines::new(&gpu.device);

        Self {
            gpu,
            config,
            pipelines,
            upload: None,
            scaled: None,
            mapped: None,
            scaler: None,
            mapper: None,
            conversion: None,
            encoder: None,
            input: None,
            bitrate_kbps: None,
        }
    }

    fn upload(&mut self, frame: &Frame) -> Result<(), HostError> {
        let data = match &frame.buffer {
            FrameBuffer::Shm { data, stride } => {
                check_shm_frame(
                    data,
                    *stride,
                    frame.resolution.width,
                    frame.resolution.height,
                )?;
                (data, *stride)
            }
            FrameBuffer::DmaBuf { .. } => {
                return Err(HostError::DmaBufCaptureUnsupported {
                    fourcc: frame.format,
                });
            }
        };
        let format = texture_format_for(frame.format)?;
        let upload = ensure_upload(&self.gpu.device, &mut self.upload, frame.resolution, format);

        self.gpu.queue.write_texture(
            upload.texture.as_image_copy(),
            data.0,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(data.1),
                rows_per_image: Some(frame.resolution.height),
            },
            extent(frame.resolution),
        );

        Ok(())
    }

    fn convert(&mut self, capture: Resolution) -> Result<(), HostError> {
        let video = &self.config.video;
        let stream = video.resolution.unwrap_or(capture);
        let device = &self.gpu.device;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rusplay-host-convert"),
        });

        let Some(upload) = &self.upload else {
            return Ok(());
        };
        let mut current = &upload.view;

        if stream != capture {
            let settings = scale_settings(&self.config, capture, stream);
            let scaler = ensure_scaler(&self.pipelines, device, &mut self.scaler, settings)?;
            let target = ensure_linear(device, &mut self.scaled, stream, "rusplay-host-scaled");
            scaler.encode(device, &mut encoder, current, &target.view);
            current = &target.view;
        }

        if video.color.primaries != ColorPrimaries::Bt709 {
            let settings = map_settings(&self.config);
            let mapper = ensure_mapper(&self.pipelines, device, &mut self.mapper, settings)?;
            let target = ensure_linear(device, &mut self.mapped, stream, "rusplay-host-mapped");
            mapper.encode(device, &mut encoder, current, &target.view);
            current = &target.view;
        }

        let settings = convert_settings(&self.config, stream);
        let conversion =
            ensure_conversion(&self.pipelines, device, &mut self.conversion, settings)?;
        conversion
            .converter
            .encode(device, &mut encoder, current, &conversion.output)?;
        encoder.copy_buffer_to_buffer(
            conversion.output.buffer(),
            0,
            &conversion.readback,
            0,
            conversion.output.layout().size,
        );
        self.gpu.queue.submit([encoder.finish()]);

        Ok(())
    }

    fn read_back(&mut self, timestamp: Timestamp) -> Result<(), HostError> {
        let Some(conversion) = &self.conversion else {
            return Ok(());
        };
        let (done, mapped) = mpsc::channel();
        conversion
            .readback
            .map_async(wgpu::MapMode::Read, .., move |result| {
                done.send(result).ok();
            });
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(HostError::GpuPoll)?;
        if let Ok(result) = mapped.recv() {
            result.map_err(HostError::GpuReadback)?;
        }

        let layout = conversion.converter.layout();
        let settings = conversion.converter.settings();
        let stride = u32::try_from(conversion.planes.luma_stride).unwrap_or(u32::MAX);
        let color = self.config.video.color;
        let input = self.input.get_or_insert_with(|| Frame {
            resolution: settings.resolution,
            format: fourcc_for(settings.format),
            color,
            timestamp,
            buffer: FrameBuffer::Shm {
                data: Vec::new(),
                stride,
            },
        });
        input.resolution = settings.resolution;
        input.format = fourcc_for(settings.format);
        input.timestamp = timestamp;

        let repacked = {
            let view = conversion
                .readback
                .get_mapped_range(..)
                .map_err(HostError::GpuMappedRange)?;
            if let FrameBuffer::Shm { data, stride: held } = &mut input.buffer {
                conversion.planes.repack(layout, &view, data);
                *held = stride;
            }
            Ok(())
        };
        conversion.readback.unmap();

        repacked
    }

    fn encode_input(&mut self, keyframe: bool) -> Result<Option<EncodedFrame>, HostError> {
        let Some(input) = &self.input else {
            return Ok(None);
        };

        let encoder = match self.encoder.take() {
            Some(open) if open.resolution == input.resolution => open,
            _ => {
                let mut encoder = rusplay_encode::open(&self.config.video)?;
                if let Some(kbps) = self.bitrate_kbps {
                    encoder.set_bitrate(kbps)?;
                }
                OpenEncoder {
                    resolution: input.resolution,
                    encoder,
                }
            }
        };
        let encoder = self.encoder.insert(encoder);

        if keyframe {
            encoder.encoder.request_keyframe();
        }
        Ok(encoder.encoder.encode(input)?)
    }
}

impl VideoPipeline for GpuPipeline {
    fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Option<EncodedFrame>, HostError> {
        self.upload(frame)?;
        self.convert(frame.resolution)?;
        self.read_back(frame.timestamp)?;
        self.encode_input(keyframe)
    }

    fn repeat(
        &mut self,
        timestamp: Timestamp,
        keyframe: bool,
    ) -> Result<Option<EncodedFrame>, HostError> {
        if let Some(input) = &mut self.input {
            input.timestamp = timestamp;
        }
        self.encode_input(keyframe)
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<(), HostError> {
        self.bitrate_kbps = Some(kbps);
        if let Some(open) = &mut self.encoder {
            open.encoder.set_bitrate(kbps)?;
        }
        Ok(())
    }

    fn stream_format(&self) -> Option<StreamFormat> {
        let video = &self.config.video;

        self.encoder.as_ref().map(|open| StreamFormat {
            codec: video.codec,
            resolution: open.resolution,
            format: video.pixel_format,
            color: video.color,
        })
    }
}

fn extent(resolution: Resolution) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: resolution.width,
        height: resolution.height,
        depth_or_array_layers: 1,
    }
}

fn ensure_upload<'a>(
    device: &wgpu::Device,
    slot: &'a mut Option<Upload>,
    resolution: Resolution,
    format: wgpu::TextureFormat,
) -> &'a Upload {
    let upload = slot
        .take()
        .filter(|upload| upload.resolution == resolution && upload.format == format)
        .unwrap_or_else(|| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rusplay-host-capture"),
                size: extent(resolution),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            Upload {
                resolution,
                format,
                texture,
                view,
            }
        });

    slot.insert(upload)
}

fn ensure_linear<'a>(
    device: &wgpu::Device,
    slot: &'a mut Option<Linear>,
    resolution: Resolution,
    label: &str,
) -> &'a Linear {
    let linear = slot
        .take()
        .filter(|linear| linear.resolution == resolution)
        .unwrap_or_else(|| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent(resolution),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: LINEAR_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            Linear {
                resolution,
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            }
        });

    slot.insert(linear)
}

fn ensure_scaler<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<Scaler>,
    settings: ScaleSettings,
) -> Result<&'a mut Scaler, HostError> {
    let scaler = match slot.take() {
        Some(mut scaler) => {
            scaler.configure(device, settings)?;
            scaler
        }
        None => Scaler::new(pipelines, device, settings)?,
    };

    Ok(slot.insert(scaler))
}

fn ensure_mapper<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<ColorMapper>,
    settings: ColorMapSettings,
) -> Result<&'a mut ColorMapper, HostError> {
    let mapper = match slot.take() {
        Some(mut mapper) => {
            mapper.configure(device, settings)?;
            mapper
        }
        None => ColorMapper::new(pipelines, device, settings)?,
    };

    Ok(slot.insert(mapper))
}

fn ensure_conversion<'a>(
    pipelines: &ScalePipelines,
    device: &wgpu::Device,
    slot: &'a mut Option<Conversion>,
    settings: ConvertSettings,
) -> Result<&'a mut Conversion, HostError> {
    let conversion = match slot.take() {
        Some(conversion) if conversion.converter.settings() == settings => conversion,
        Some(Conversion { mut converter, .. }) => {
            converter.configure(device, settings)?;
            conversion_buffers(device, converter, settings)
        }
        None => {
            let converter = ColorConverter::new(pipelines, device, settings)?;
            conversion_buffers(device, converter, settings)
        }
    };

    Ok(slot.insert(conversion))
}

fn conversion_buffers(
    device: &wgpu::Device,
    converter: ColorConverter,
    settings: ConvertSettings,
) -> Conversion {
    let output = converter.create_frame(device);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rusplay-host-readback"),
        size: converter.layout().size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let planes = EncoderPlanes::of(converter.layout(), settings.format.chroma);

    Conversion {
        converter,
        output,
        readback,
        planes,
    }
}

fn scale_settings(config: &Config, source: Resolution, target: Resolution) -> ScaleSettings {
    let scaling = &config.scaling;
    let upscaling = target.width > source.width || target.height > source.height;

    if upscaling {
        let upscale = &scaling.upscale;
        ScaleSettings {
            filter: upscale.filter,
            source,
            target,
            antiringing: upscale.antiringing,
            sigmoid: upscale.sigmoid.then_some(Sigmoid {
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

fn map_settings(config: &Config) -> ColorMapSettings {
    ColorMapSettings {
        source: ColorVolume::srgb_display(),
        target: ColorVolume::of_content(&config.video.color),
        tone: ToneMapping::default(),
        gamut: GamutMapping {
            mode: config.color.gamut_mapping.mode,
            knee: f64::from(config.color.gamut_mapping.knee),
        },
    }
}

fn convert_settings(config: &Config, resolution: Resolution) -> ConvertSettings {
    let video = &config.video;

    ConvertSettings {
        resolution,
        format: video.pixel_format,
        layout: PlaneLayout::Planar,
        matrix: video.color.matrix_coefficients(),
        range: video.color.range,
        transfer: video.color.transfer,
        hdr: video.color.hdr,
        input: InputLight::Linear,
        chroma_filter: config.scaling.downscale.chroma_filter,
        chroma_siting: ChromaSiting::Left,
        row_alignment: CONVERT_ROW_ALIGNMENT,
    }
}
