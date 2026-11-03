use std::{ffi::c_int, ptr};

use libloading::Library;

use crate::{
    error::{CudaCall, CudaError, CudaStatus},
    ffi::{
        CUDA_SUCCESS, CuContext, CuCtxPopCurrentFn, CuCtxPushCurrentFn, CuDevice,
        CuDeviceGetCountFn, CuDeviceGetFn, CuInitFn, CuResult,
    },
};

pub const CUDA_LIBRARY: &str = "libcuda.so.1";

pub fn check(call: CudaCall, result: CuResult) -> Result<(), CudaError> {
    if result == CUDA_SUCCESS {
        Ok(())
    } else {
        Err(CudaError::Call {
            call,
            status: CudaStatus(result),
        })
    }
}

pub fn open_library(library: &'static str) -> Result<Library, CudaError> {
    unsafe { Library::new(library) }
        .map_err(|source| CudaError::LibraryNotFound { library, source })
}

pub fn symbol<T: Copy>(
    library: &Library,
    name: &'static str,
    symbol: &'static str,
) -> Result<T, CudaError> {
    unsafe { library.get::<T>(symbol) }
        .map(|found| *found)
        .map_err(|source| CudaError::SymbolMissing {
            library: name,
            symbol,
            source,
        })
}

#[derive(Debug, Clone, Copy)]
pub struct ContextHandle(CuContext);

impl ContextHandle {
    pub const fn new(raw: CuContext) -> Self {
        Self(raw)
    }
}

pub struct Driver {
    name: &'static str,
    init: CuInitFn,
    device_get_count: CuDeviceGetCountFn,
    device_get: CuDeviceGetFn,
    ctx_push_current: CuCtxPushCurrentFn,
    ctx_pop_current: CuCtxPopCurrentFn,
    library: Library,
}

impl Driver {
    pub fn load() -> Result<Self, CudaError> {
        Self::load_from(CUDA_LIBRARY)
    }

    pub fn load_from(name: &'static str) -> Result<Self, CudaError> {
        let library = open_library(name)?;

        Ok(Self {
            name,
            init: symbol(&library, name, "cuInit")?,
            device_get_count: symbol(&library, name, "cuDeviceGetCount")?,
            device_get: symbol(&library, name, "cuDeviceGet")?,
            ctx_push_current: symbol(&library, name, "cuCtxPushCurrent_v2")?,
            ctx_pop_current: symbol(&library, name, "cuCtxPopCurrent_v2")?,
            library,
        })
    }

    pub fn symbol<T: Copy>(&self, symbol_name: &'static str) -> Result<T, CudaError> {
        symbol(&self.library, self.name, symbol_name)
    }

    pub fn device(&self, index: u32) -> Result<CuDevice, CudaError> {
        check(CudaCall::Init, unsafe { (self.init)(0) })?;

        let mut count: c_int = 0;
        check(CudaCall::DeviceGetCount, unsafe {
            (self.device_get_count)(&raw mut count)
        })?;
        let count = u32::try_from(count).unwrap_or(0);
        let ordinal = c_int::try_from(index)
            .ok()
            .filter(|_| index < count)
            .ok_or(CudaError::DeviceMissing { index, count })?;

        let mut device: CuDevice = 0;
        check(CudaCall::DeviceGet, unsafe {
            (self.device_get)(&raw mut device, ordinal)
        })?;

        Ok(device)
    }

    pub fn push_current(&self, context: ContextHandle) -> Result<Current<'_>, CudaError> {
        check(CudaCall::CtxPushCurrent, unsafe {
            (self.ctx_push_current)(context.0)
        })?;

        Ok(Current { driver: self })
    }

    pub fn pop_current(&self) -> Result<(), CudaError> {
        let mut popped: CuContext = ptr::null_mut();

        check(CudaCall::CtxPopCurrent, unsafe {
            (self.ctx_pop_current)(&raw mut popped)
        })
    }
}

pub struct Current<'a> {
    driver: &'a Driver,
}

impl Drop for Current<'_> {
    fn drop(&mut self) {
        let mut popped: CuContext = ptr::null_mut();
        unsafe { (self.driver.ctx_pop_current)(&raw mut popped) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_library_is_named() {
        let Err(error) = Driver::load_from("librusplay-absent-cuda.so.1") else {
            panic!("an absent library loaded");
        };

        assert!(matches!(
            error,
            CudaError::LibraryNotFound { library, .. } if library == "librusplay-absent-cuda.so.1"
        ));
    }

    #[test]
    fn a_missing_symbol_is_a_specific_error() {
        let Ok(library) = open_library("libc.so.6") else {
            return;
        };

        let result = symbol::<CuInitFn>(&library, "libc.so.6", "rusplayMissingSymbol");

        assert!(matches!(
            result,
            Err(CudaError::SymbolMissing {
                library: "libc.so.6",
                symbol: "rusplayMissingSymbol",
                ..
            })
        ));
    }

    #[test]
    fn a_failed_call_carries_its_status() {
        let error = check(CudaCall::ImportExternalMemory, 801).unwrap_err();

        assert_eq!(
            error.to_string(),
            "cuImportExternalMemory failed with not supported (801)"
        );
        assert!(check(CudaCall::Init, CUDA_SUCCESS).is_ok());
    }
}
