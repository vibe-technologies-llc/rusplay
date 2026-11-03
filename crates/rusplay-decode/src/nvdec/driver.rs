use std::{ptr, rc::Rc};

use rusplay_cuda::{
    ContextHandle, CudaCall, CudaStatus, Current, Driver as CudaDriver, Library,
    check as cuda_check,
    ffi::{CUDA_SUCCESS, CuContext, CuDevice, CuResult},
    open_library, symbol,
};

use crate::{CuvidCall, DecodeError, nvdec::ffi};

pub(super) const NVCUVID_LIBRARY: &str = "libnvcuvid.so.1";

pub(super) fn check(call: CuvidCall, result: CuResult) -> Result<(), DecodeError> {
    if result == CUDA_SUCCESS {
        Ok(())
    } else {
        Err(DecodeError::Cuvid {
            call,
            status: CudaStatus(result),
        })
    }
}

pub(super) struct Cuda {
    pub driver: CudaDriver,
    pub primary_ctx_retain: ffi::CuDevicePrimaryCtxRetain,
    pub primary_ctx_release: ffi::CuDevicePrimaryCtxRelease,
    pub memcpy_2d: ffi::CuMemcpy2D,
}

impl Cuda {
    fn load() -> Result<Self, DecodeError> {
        let driver = CudaDriver::load()?;

        Ok(Self {
            primary_ctx_retain: driver.symbol("cuDevicePrimaryCtxRetain")?,
            primary_ctx_release: driver.symbol("cuDevicePrimaryCtxRelease")?,
            memcpy_2d: driver.symbol("cuMemcpy2D_v2")?,
            driver,
        })
    }
}

pub(super) struct Cuvid {
    pub get_decoder_caps: ffi::CuvidGetDecoderCaps,
    pub create_decoder: ffi::CuvidCreateDecoder,
    pub destroy_decoder: ffi::CuvidDestroyDecoder,
    pub decode_picture: ffi::CuvidDecodePicture,
    pub map_video_frame: ffi::CuvidMapVideoFrame,
    pub unmap_video_frame: ffi::CuvidUnmapVideoFrame,
    pub create_video_parser: ffi::CuvidCreateVideoParser,
    pub parse_video_data: ffi::CuvidParseVideoData,
    pub destroy_video_parser: ffi::CuvidDestroyVideoParser,
    _library: Library,
}

impl Cuvid {
    fn load(library: Library) -> Result<Self, DecodeError> {
        let name = NVCUVID_LIBRARY;

        Ok(Self {
            get_decoder_caps: symbol(&library, name, "cuvidGetDecoderCaps")?,
            create_decoder: symbol(&library, name, "cuvidCreateDecoder")?,
            destroy_decoder: symbol(&library, name, "cuvidDestroyDecoder")?,
            decode_picture: symbol(&library, name, "cuvidDecodePicture")?,
            map_video_frame: symbol(&library, name, "cuvidMapVideoFrame64")?,
            unmap_video_frame: symbol(&library, name, "cuvidUnmapVideoFrame64")?,
            create_video_parser: symbol(&library, name, "cuvidCreateVideoParser")?,
            parse_video_data: symbol(&library, name, "cuvidParseVideoData")?,
            destroy_video_parser: symbol(&library, name, "cuvidDestroyVideoParser")?,
            _library: library,
        })
    }
}

pub(super) struct Driver {
    pub cuda: Cuda,
    pub cuvid: Cuvid,
}

impl Driver {
    pub(super) fn load() -> Result<Rc<Self>, DecodeError> {
        let cuda = Cuda::load()?;
        let cuvid = Cuvid::load(open_library(NVCUVID_LIBRARY)?)?;

        Ok(Rc::new(Self { cuda, cuvid }))
    }
}

pub(super) struct Context {
    driver: Rc<Driver>,
    device: CuDevice,
    primary: CuContext,
}

impl Context {
    pub(super) fn open(driver: &Rc<Driver>, index: u32) -> Result<Rc<Self>, DecodeError> {
        let cuda = &driver.cuda;
        let device = cuda.driver.device(index)?;

        let mut context: CuContext = ptr::null_mut();
        cuda_check(CudaCall::DevicePrimaryCtxRetain, unsafe {
            (cuda.primary_ctx_retain)(&raw mut context, device)
        })?;

        Ok(Rc::new(Self {
            driver: Rc::clone(driver),
            device,
            primary: context,
        }))
    }

    pub(super) fn driver(&self) -> &Rc<Driver> {
        &self.driver
    }

    pub(super) fn make_current(&self) -> Result<Current<'_>, DecodeError> {
        Ok(self
            .driver
            .cuda
            .driver
            .push_current(ContextHandle::new(self.primary))?)
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { (self.driver.cuda.primary_ctx_release)(self.device) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_cuvid_call_carries_its_status() {
        let result = check(CuvidCall::CreateDecoder, 100);

        assert!(matches!(
            result,
            Err(DecodeError::Cuvid {
                call: CuvidCall::CreateDecoder,
                status: CudaStatus(100)
            })
        ));
        assert!(check(CuvidCall::CreateDecoder, CUDA_SUCCESS).is_ok());
    }
}
