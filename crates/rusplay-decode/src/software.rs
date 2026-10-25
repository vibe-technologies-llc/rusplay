use std::{ffi::c_int, fmt, io, mem::MaybeUninit, ptr::NonNull, slice};

use rav1d::{
    Dav1dResult,
    include::dav1d::{
        data::Dav1dData,
        dav1d::{Dav1dContext, Dav1dSettings},
        headers::{DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422, DAV1D_PIXEL_LAYOUT_I444},
        picture::Dav1dPicture,
    },
    src::lib::{
        dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings,
        dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data,
    },
};
use rusplay_config::SoftwareDecoderConfig;
use rusplay_core::{
    BitDepth, ChromaSubsampling, Codec, EncodedFrame, PixelFormat, Resolution, SignalledColor,
    Timestamp,
};

use crate::{
    Backend, CpuFrame, DecodeError, DecodedBuffer, DecodedFrame, VideoDecoder,
    frame::{CpuPlane, PlaneStorage},
    metadata::{av1_mastering_display, content_light},
};

const AUTOMATIC_THREAD_COUNT: c_int = 0;
const ONE_FRAME_IN_FLIGHT: c_int = 1;

pub struct Rav1dDecoder {
    context: Option<Dav1dContext>,
}

impl fmt::Debug for Rav1dDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rav1dDecoder").finish_non_exhaustive()
    }
}

impl Rav1dDecoder {
    pub fn new(config: &SoftwareDecoderConfig) -> Result<Self, DecodeError> {
        let threads = match config.threads {
            Some(threads) => c_int::try_from(threads.get())
                .map_err(|_| DecodeError::DecoderThreadsOutOfRange(threads.get()))?,
            None => AUTOMATIC_THREAD_COUNT,
        };

        let mut settings = default_settings();
        settings.n_threads = threads;
        settings.max_frame_delay = ONE_FRAME_IN_FLIGHT;

        let mut context = None;
        let result = unsafe {
            dav1d_open(
                Some(NonNull::from(&mut context)),
                Some(NonNull::from(&mut settings)),
            )
        };
        check(result).map_err(DecodeError::Rav1dOpen)?;

        Ok(Self { context })
    }

    fn send(&self, data: &mut InputData) -> Result<SendOutcome, DecodeError> {
        let result = unsafe { dav1d_send_data(self.context, Some(NonNull::from(&mut data.0))) };
        match check(result) {
            Ok(()) => Ok(SendOutcome::Accepted),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(SendOutcome::NeedsDrain),
            Err(error) => Err(DecodeError::Rav1dSendData(error)),
        }
    }

    fn next_picture(&self) -> Result<Option<OwnedPicture>, DecodeError> {
        let mut picture = OwnedPicture(Dav1dPicture::default());
        let result =
            unsafe { dav1d_get_picture(self.context, Some(NonNull::from(&mut picture.0))) };
        match check(result) {
            Ok(()) => Ok(Some(picture)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(DecodeError::Rav1dGetPicture(error)),
        }
    }
}

impl Drop for Rav1dDecoder {
    fn drop(&mut self) {
        unsafe { dav1d_close(Some(NonNull::from(&mut self.context))) };
    }
}

impl VideoDecoder for Rav1dDecoder {
    fn decode(&mut self, frame: &EncodedFrame) -> Result<Option<DecodedFrame>, DecodeError> {
        if frame.codec != Codec::Av1 {
            return Err(DecodeError::CodecMismatch {
                expected: Codec::Av1,
                found: frame.codec,
            });
        }

        if frame.data.is_empty() {
            return Ok(None);
        }

        let mut data = InputData::copy_from(&frame.data, frame.timestamp)?;
        let mut latest = None;

        while data.remaining() > 0 {
            match self.send(&mut data)? {
                SendOutcome::Accepted => {}
                SendOutcome::NeedsDrain => match self.next_picture()? {
                    Some(picture) => latest = Some(picture),
                    None => break,
                },
            }
        }

        if let Some(picture) = self.next_picture()? {
            latest = Some(picture);
        }

        latest.map(OwnedPicture::into_frame).transpose()
    }
}

fn display_resolution(coded: Resolution, render: Option<Resolution>) -> Resolution {
    match render {
        Some(render) if render.width > 0 && render.height > 0 => Resolution::new(
            render.width.min(coded.width),
            render.height.min(coded.height),
        ),
        _ => coded,
    }
}

enum SendOutcome {
    Accepted,
    NeedsDrain,
}

fn default_settings() -> Dav1dSettings {
    let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
    unsafe {
        dav1d_default_settings(NonNull::from(&mut settings).cast());
        settings.assume_init()
    }
}

fn check(result: Dav1dResult) -> Result<(), io::Error> {
    if result.0 < 0 {
        Err(io::Error::from_raw_os_error(-result.0))
    } else {
        Ok(())
    }
}

struct InputData(Dav1dData);

impl InputData {
    fn copy_from(bytes: &[u8], timestamp: Timestamp) -> Result<Self, DecodeError> {
        let mut data = Self(Dav1dData::default());
        let destination =
            unsafe { dav1d_data_create(Some(NonNull::from(&mut data.0)), bytes.len()) };
        if destination.is_null() {
            return Err(DecodeError::Rav1dDataAlloc { size: bytes.len() });
        }

        unsafe { destination.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
        data.0.m.timestamp = i64::try_from(timestamp.as_micros()).unwrap_or(i64::MAX);

        Ok(data)
    }

    fn remaining(&self) -> usize {
        self.0.sz
    }
}

impl Drop for InputData {
    fn drop(&mut self) {
        unsafe { dav1d_data_unref(Some(NonNull::from(&mut self.0))) };
    }
}

struct OwnedPicture(Dav1dPicture);

unsafe impl Send for OwnedPicture {}
unsafe impl Sync for OwnedPicture {}

impl fmt::Debug for OwnedPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedPicture")
            .field("width", &self.0.p.w)
            .field("height", &self.0.p.h)
            .field("bpc", &self.0.p.bpc)
            .finish_non_exhaustive()
    }
}

impl Drop for OwnedPicture {
    fn drop(&mut self) {
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut self.0))) };
    }
}

impl OwnedPicture {
    fn chroma(&self) -> Result<ChromaSubsampling, DecodeError> {
        match self.0.p.layout {
            DAV1D_PIXEL_LAYOUT_I420 => Ok(ChromaSubsampling::Yuv420),
            DAV1D_PIXEL_LAYOUT_I422 => Ok(ChromaSubsampling::Yuv422),
            DAV1D_PIXEL_LAYOUT_I444 => Ok(ChromaSubsampling::Yuv444),
            _ => Err(DecodeError::Monochrome {
                backend: Backend::Software,
                codec: Codec::Av1,
            }),
        }
    }

    fn depth(&self) -> Result<BitDepth, DecodeError> {
        match self.0.p.bpc {
            8 => Ok(BitDepth::Eight),
            10 => Ok(BitDepth::Ten),
            12 => Ok(BitDepth::Twelve),
            other => Err(DecodeError::BitDepthUnsupported {
                backend: Backend::Software,
                codec: Codec::Av1,
                bits: u8::try_from(other).unwrap_or(u8::MAX),
            }),
        }
    }

    fn resolution(&self) -> Resolution {
        let coded = Resolution::new(
            u32::try_from(self.0.p.w).unwrap_or_default(),
            u32::try_from(self.0.p.h).unwrap_or_default(),
        );
        let render = self.0.frame_hdr.map(|header| {
            let header = unsafe { header.as_ref() };
            Resolution::new(
                u32::try_from(header.render_width).unwrap_or_default(),
                u32::try_from(header.render_height).unwrap_or_default(),
            )
        });

        display_resolution(coded, render)
    }

    fn color(&self) -> SignalledColor {
        let mut color = SignalledColor::default();

        if let Some(sequence) = self.0.seq_hdr {
            let sequence = unsafe { sequence.as_ref() };
            let code = |value: u32| u8::try_from(value).unwrap_or(u8::MAX);
            color = color
                .with_h273_codes(code(sequence.pri), code(sequence.trc), code(sequence.mtrx))
                .with_full_range(sequence.color_range != 0);
        }
        if let Some(display) = self.0.mastering_display {
            let display = unsafe { display.as_ref() };
            color.mastering = av1_mastering_display(
                display.primaries,
                display.white_point,
                display.max_luminance,
                display.min_luminance,
            );
        }
        if let Some(light) = self.0.content_light {
            let light = unsafe { light.as_ref() };
            color.content_light = Some(content_light(
                light.max_content_light_level,
                light.max_frame_average_light_level,
            ));
        }

        color
    }

    fn into_frame(self) -> Result<DecodedFrame, DecodeError> {
        let format = PixelFormat {
            chroma: self.chroma()?,
            depth: self.depth()?,
        };
        let resolution = self.resolution();
        let timestamp = Timestamp::from_micros(u64::try_from(self.0.m.timestamp).unwrap_or(0));

        Ok(DecodedFrame {
            resolution,
            format,
            color: self.color(),
            timestamp,
            buffer: DecodedBuffer::Cpu(CpuFrame::new(PictureStorage {
                picture: self,
                chroma: format.chroma,
                bytes_per_sample: if format.depth == BitDepth::Eight {
                    1
                } else {
                    2
                },
                resolution,
            })),
        })
    }
}

#[derive(Debug)]
struct PictureStorage {
    picture: OwnedPicture,
    chroma: ChromaSubsampling,
    bytes_per_sample: u8,
    resolution: Resolution,
}

impl PictureStorage {
    const PLANES: usize = 3;

    fn plane_size(&self, index: usize) -> (u32, u32) {
        let Resolution { width, height } = self.resolution;
        if index == 0 {
            return (width, height);
        }

        match self.chroma {
            ChromaSubsampling::Yuv420 => (width.div_ceil(2), height.div_ceil(2)),
            ChromaSubsampling::Yuv422 => (width.div_ceil(2), height),
            ChromaSubsampling::Yuv444 => (width, height),
        }
    }
}

impl PlaneStorage for PictureStorage {
    fn plane_count(&self) -> usize {
        Self::PLANES
    }

    fn plane(&self, index: usize) -> Option<CpuPlane<'_>> {
        let base = (*self.picture.0.data.get(index)?)?;
        let stride = usize::try_from(self.picture.0.stride[index.min(1)]).ok()?;
        let (width, height) = self.plane_size(index);
        let length = stride.checked_mul(usize::try_from(height).ok()?)?;
        let data = unsafe { slice::from_raw_parts(base.cast::<u8>().as_ptr(), length) };

        Some(CpuPlane {
            data,
            stride,
            width,
            height,
            bytes_per_sample: self.bytes_per_sample,
        })
    }
}

#[cfg(test)]
mod tests;
