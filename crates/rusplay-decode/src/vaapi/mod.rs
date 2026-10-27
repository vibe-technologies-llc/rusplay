mod av1;
mod capabilities;
mod ffi;
mod h264;
mod hevc;
mod reorder;
#[cfg(test)]
mod tests;
mod va;

use std::{fmt, rc::Rc, sync::Arc};

use rusplay_config::VaapiDecoderConfig;
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution, SignalledColor,
    Timestamp,
};

pub(crate) use crate::vaapi::capabilities::probe as capabilities;
use crate::{
    Backend, DecodeError, DecodedBuffer, DecodedFrame, DmaBufFrame, VideoDecoder,
    vaapi::va::{Display, PoolSurface, Profile, Session, SurfacePool},
};

const OUTPUT_FRAMES_IN_FLIGHT: usize = 8;

pub struct VaapiDecoder {
    codec: Codec,
    display: Rc<Display>,
    state: CodecState,
}

enum CodecState {
    H264(Box<h264::H264Decoder>),
    Hevc(Box<hevc::HevcDecoder>),
    Av1(Box<av1::Av1Decoder>),
}

impl fmt::Debug for VaapiDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaapiDecoder")
            .field("codec", &self.codec)
            .finish_non_exhaustive()
    }
}

impl VaapiDecoder {
    pub fn new(codec: Codec, config: &VaapiDecoderConfig) -> Result<Self, DecodeError> {
        let display = Display::open(&config.device)?;

        let base_profiles: &[Profile] = match codec {
            Codec::H264 => &[
                ffi::VAProfile_VAProfileH264ConstrainedBaseline,
                ffi::VAProfile_VAProfileH264Main,
                ffi::VAProfile_VAProfileH264High,
            ],
            Codec::Hevc => &[ffi::VAProfile_VAProfileHEVCMain],
            Codec::Av1 => &[ffi::VAProfile_VAProfileAV1Profile0],
        };

        let mut supported = false;
        for &profile in base_profiles {
            supported |= display.has_decoder(profile)?;
        }
        if !supported {
            return Err(DecodeError::CodecUnsupported {
                backend: Backend::Vaapi,
                codec,
            });
        }

        let state = match codec {
            Codec::H264 => CodecState::H264(Box::new(h264::H264Decoder::new())),
            Codec::Hevc => CodecState::Hevc(Box::new(hevc::HevcDecoder::new())),
            Codec::Av1 => CodecState::Av1(Box::new(av1::Av1Decoder::new())),
        };

        Ok(Self {
            codec,
            display,
            state,
        })
    }
}

#[cfg(test)]
impl VaapiDecoder {
    fn display(&self) -> &Rc<Display> {
        &self.display
    }
}

impl VideoDecoder for VaapiDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<DecodedFrame>, DecodeError> {
        if frame.codec != self.codec {
            return Err(DecodeError::CodecMismatch {
                expected: self.codec,
                found: frame.codec,
            });
        }

        match &mut self.state {
            CodecState::H264(decoder) => decoder.decode(&self.display, frame),
            CodecState::Hevc(decoder) => decoder.decode(&self.display, frame),
            CodecState::Av1(decoder) => decoder.decode(&self.display, frame),
        }
    }

    fn flush(&mut self) -> Result<Vec<DecodedFrame>, DecodeError> {
        Ok(match &mut self.state {
            CodecState::H264(decoder) => decoder.drain()?,
            CodecState::Hevc(decoder) => decoder.drain(),
            CodecState::Av1(_) => Vec::new(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamFormat {
    profile: Profile,
    profile_idc: u8,
    pixel_format: PixelFormat,
    coded: Resolution,
    visible: Resolution,
    reference_frames: usize,
    reorder_frames: usize,
}

impl StreamFormat {
    fn rt_format(&self) -> u32 {
        rt_format(self.pixel_format)
    }

    fn unsupported(&self, codec: Codec) -> DecodeError {
        if self.pixel_format.chroma != ChromaSubsampling::Yuv420 {
            DecodeError::ChromaUnsupported {
                backend: Backend::Vaapi,
                codec,
                chroma: self.pixel_format.chroma,
            }
        } else if self.pixel_format.depth != BitDepth::Eight {
            DecodeError::BitDepthUnsupported {
                backend: Backend::Vaapi,
                codec,
                bits: self.pixel_format.depth.bits(),
            }
        } else {
            DecodeError::ProfileUnsupported {
                codec,
                profile: self.profile_idc,
            }
        }
    }
}

const fn rt_format(format: PixelFormat) -> u32 {
    match (format.chroma, format.depth) {
        (ChromaSubsampling::Yuv420, BitDepth::Eight) => ffi::VA_RT_FORMAT_YUV420,
        (ChromaSubsampling::Yuv420, BitDepth::Ten) => ffi::VA_RT_FORMAT_YUV420_10,
        (ChromaSubsampling::Yuv420, BitDepth::Twelve) => ffi::VA_RT_FORMAT_YUV420_12,
        (ChromaSubsampling::Yuv422, BitDepth::Eight) => ffi::VA_RT_FORMAT_YUV422,
        (ChromaSubsampling::Yuv422, BitDepth::Ten) => ffi::VA_RT_FORMAT_YUV422_10,
        (ChromaSubsampling::Yuv422, BitDepth::Twelve) => ffi::VA_RT_FORMAT_YUV422_12,
        (ChromaSubsampling::Yuv444, BitDepth::Eight) => ffi::VA_RT_FORMAT_YUV444,
        (ChromaSubsampling::Yuv444, BitDepth::Ten) => ffi::VA_RT_FORMAT_YUV444_10,
        (ChromaSubsampling::Yuv444, BitDepth::Twelve) => ffi::VA_RT_FORMAT_YUV444_12,
    }
}

struct Stream {
    format: StreamFormat,
    color: SignalledColor,
    pool: SurfacePool,
    session: Session,
}

impl Stream {
    fn ensure<'a>(
        slot: &'a mut Option<Self>,
        display: &Rc<Display>,
        codec: Codec,
        format: StreamFormat,
    ) -> Result<(&'a mut Self, bool), DecodeError> {
        let (stream, reopened) = match slot.take() {
            Some(stream) if stream.format == format => (stream, false),
            previous => {
                drop(previous);
                (Self::open(display, codec, format)?, true)
            }
        };

        Ok((slot.insert(stream), reopened))
    }

    fn open(
        display: &Rc<Display>,
        codec: Codec,
        format: StreamFormat,
    ) -> Result<Self, DecodeError> {
        let rt_format = format.rt_format();

        if !display.has_decoder(format.profile)?
            || display.rt_formats(format.profile)? & rt_format == 0
        {
            return Err(format.unsupported(codec));
        }

        let session = Session::new(
            display,
            format.profile,
            rt_format,
            format.coded.width,
            format.coded.height,
        )?;
        let pool = SurfacePool::new(
            display,
            rt_format,
            format.coded.width,
            format.coded.height,
            format.reference_frames + format.reorder_frames + 1 + OUTPUT_FRAMES_IN_FLIGHT,
        );

        Ok(Self {
            format,
            color: SignalledColor::default(),
            pool,
            session,
        })
    }

    fn output(
        &self,
        surface: &PoolSurface,
        timestamp: Timestamp,
    ) -> Result<DecodedFrame, DecodeError> {
        self.output_sized(surface, timestamp, self.format.visible)
    }

    fn output_sized(
        &self,
        surface: &PoolSurface,
        timestamp: Timestamp,
        resolution: Resolution,
    ) -> Result<DecodedFrame, DecodeError> {
        self.session.display().sync(surface.id)?;

        Ok(DecodedFrame {
            resolution,
            format: self.format.pixel_format,
            color: self.color,
            timestamp,
            buffer: DecodedBuffer::DmaBuf(DmaBufFrame::new(Arc::clone(&surface.image), ())),
        })
    }
}
