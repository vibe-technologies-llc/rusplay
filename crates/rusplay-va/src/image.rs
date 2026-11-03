use std::{ptr, rc::Rc, slice};

use rusplay_core::Resolution;

use crate::{
    display::{Display, c_int_of, check, usize_of},
    error::{VaCall, VaError},
    ffi,
    surface::Surface,
};

#[derive(Debug, Clone, Copy)]
pub struct ImageLayout {
    pub offsets: [usize; 3],
    pub pitches: [usize; 3],
}

pub struct Image {
    display: Rc<Display>,
    raw: ffi::VAImage,
    resolution: Resolution,
}

impl Image {
    pub fn new(
        display: &Rc<Display>,
        va_fourcc: u32,
        resolution: Resolution,
    ) -> Result<Self, VaError> {
        let mut format = ffi::VAImageFormat {
            fourcc: va_fourcc,
            ..Default::default()
        };
        let mut raw = ffi::VAImage::default();
        check(VaCall::CreateImage, unsafe {
            ffi::vaCreateImage(
                display.raw(),
                &raw mut format,
                c_int_of(resolution.width),
                c_int_of(resolution.height),
                &raw mut raw,
            )
        })?;

        Ok(Self {
            display: Rc::clone(display),
            raw,
            resolution,
        })
    }

    pub fn layout(&self) -> ImageLayout {
        ImageLayout {
            offsets: [0, 1, 2].map(|plane| usize_of(self.raw.offsets[plane])),
            pitches: [0, 1, 2].map(|plane| usize_of(self.raw.pitches[plane])),
        }
    }

    pub fn write(&mut self, fill: impl FnOnce(&mut [u8], ImageLayout)) -> Result<(), VaError> {
        let mut mapped = ptr::null_mut();
        check(VaCall::MapBuffer, unsafe {
            ffi::vaMapBuffer(self.display.raw(), self.raw.buf, &raw mut mapped)
        })?;

        let layout = self.layout();
        let data =
            unsafe { slice::from_raw_parts_mut(mapped.cast::<u8>(), usize_of(self.raw.data_size)) };
        fill(data, layout);

        check(VaCall::UnmapBuffer, unsafe {
            ffi::vaUnmapBuffer(self.display.raw(), self.raw.buf)
        })
    }

    pub fn put(&self, surface: &Surface) -> Result<(), VaError> {
        let Resolution { width, height } = self.resolution;

        check(VaCall::PutImage, unsafe {
            ffi::vaPutImage(
                self.display.raw(),
                surface.id(),
                self.raw.image_id,
                0,
                0,
                width,
                height,
                0,
                0,
                width,
                height,
            )
        })
    }

    pub fn get(&mut self, surface: &Surface) -> Result<Vec<u8>, VaError> {
        let Resolution { width, height } = self.resolution;
        check(VaCall::GetImage, unsafe {
            ffi::vaGetImage(
                self.display.raw(),
                surface.id(),
                0,
                0,
                width,
                height,
                self.raw.image_id,
            )
        })?;

        let mut copy = Vec::new();
        self.write(|data, _| copy.extend_from_slice(data))?;

        Ok(copy)
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe { ffi::vaDestroyImage(self.display.raw(), self.raw.image_id) };
    }
}
