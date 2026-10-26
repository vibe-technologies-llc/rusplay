use std::{ffi::c_int, ptr, rc::Rc};

use libloading::Library;

use crate::{
    CudaCall, CudaStatus, DecodeError,
    nvdec::ffi::{self, CuContext, CuDevice, CuResult},
};

pub(super) const CUDA_LIBRARY: &str = "libcuda.so.1";
pub(super) const NVCUVID_LIBRARY: &str = "libnvcuvid.so.1";

pub(super) fn check(call: CudaCall, result: CuResult) -> Result<(), DecodeError> {
    if result == ffi::CUDA_SUCCESS {
        Ok(())
    } else {
        Err(DecodeError::Cuda {
            call,
            status: CudaStatus(result),
        })
    }
}

pub(super) fn open_library(library: &'static str) -> Result<Library, DecodeError> {
    unsafe { Library::new(library) }
        .map_err(|source| DecodeError::LibraryUnavailable { library, source })
}

fn symbol<T: Copy>(
    library: &Library,
    name: &'static str,
    symbol: &'static str,
) -> Result<T, DecodeError> {
    unsafe { library.get::<T>(symbol) }
        .map(|loaded| *loaded)
        .map_err(|source| DecodeError::SymbolMissing {
            library: name,
            symbol,
            source,
        })
}

pub(super) struct Cuda {
    pub init: ffi::CuInit,
    pub device_get_count: ffi::CuDeviceGetCount,
    pub device_get: ffi::CuDeviceGet,
    pub primary_ctx_retain: ffi::CuDevicePrimaryCtxRetain,
    pub primary_ctx_release: ffi::CuDevicePrimaryCtxRelease,
    pub ctx_push_current: ffi::CuCtxPushCurrent,
    pub ctx_pop_current: ffi::CuCtxPopCurrent,
    pub memcpy_2d: ffi::CuMemcpy2D,
    _library: Library,
}

impl Cuda {
    fn load(library: Library) -> Result<Self, DecodeError> {
        let name = CUDA_LIBRARY;

        Ok(Self {
            init: symbol(&library, name, "cuInit")?,
            device_get_count: symbol(&library, name, "cuDeviceGetCount")?,
            device_get: symbol(&library, name, "cuDeviceGet")?,
            primary_ctx_retain: symbol(&library, name, "cuDevicePrimaryCtxRetain")?,
            primary_ctx_release: symbol(&library, name, "cuDevicePrimaryCtxRelease")?,
            ctx_push_current: symbol(&library, name, "cuCtxPushCurrent_v2")?,
            ctx_pop_current: symbol(&library, name, "cuCtxPopCurrent_v2")?,
            memcpy_2d: symbol(&library, name, "cuMemcpy2D_v2")?,
            _library: library,
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
        let cuda = Cuda::load(open_library(CUDA_LIBRARY)?)?;
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
        check(CudaCall::Init, unsafe { (cuda.init)(0) })?;

        let mut count: c_int = 0;
        check(CudaCall::DeviceGetCount, unsafe {
            (cuda.device_get_count)(&raw mut count)
        })?;
        let ordinal = c_int::try_from(index)
            .ok()
            .filter(|ordinal| *ordinal < count)
            .ok_or(DecodeError::CudaDeviceMissing {
                index,
                count: u32::try_from(count).unwrap_or(0),
            })?;

        let mut device: CuDevice = 0;
        check(CudaCall::DeviceGet, unsafe {
            (cuda.device_get)(&raw mut device, ordinal)
        })?;

        let mut context: CuContext = ptr::null_mut();
        check(CudaCall::DevicePrimaryCtxRetain, unsafe {
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
        check(CudaCall::CtxPushCurrent, unsafe {
            (self.driver.cuda.ctx_push_current)(self.primary)
        })?;

        Ok(Current { context: self })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { (self.driver.cuda.primary_ctx_release)(self.device) };
    }
}

pub(super) struct Current<'a> {
    context: &'a Context,
}

impl Drop for Current<'_> {
    fn drop(&mut self) {
        let mut popped: CuContext = ptr::null_mut();
        unsafe { (self.context.driver.cuda.ctx_pop_current)(&raw mut popped) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_library_is_a_specific_error() {
        let result = open_library("librusplay-missing-nvidia-driver.so.1");

        assert!(matches!(
            result,
            Err(DecodeError::LibraryUnavailable {
                library: "librusplay-missing-nvidia-driver.so.1",
                ..
            })
        ));
    }

    #[test]
    fn a_missing_symbol_is_a_specific_error() {
        let Ok(library) = open_library("libc.so.6") else {
            return;
        };

        let result = symbol::<ffi::CuInit>(&library, "libc.so.6", "rusplayMissingSymbol");

        assert!(matches!(
            result,
            Err(DecodeError::SymbolMissing {
                library: "libc.so.6",
                symbol: "rusplayMissingSymbol",
                ..
            })
        ));
    }

    #[test]
    fn a_failed_call_carries_its_status() {
        let result = check(CudaCall::Init, 100);

        assert!(matches!(
            result,
            Err(DecodeError::Cuda {
                call: CudaCall::Init,
                status: CudaStatus(100)
            })
        ));
        assert!(check(CudaCall::Init, ffi::CUDA_SUCCESS).is_ok());
    }
}
