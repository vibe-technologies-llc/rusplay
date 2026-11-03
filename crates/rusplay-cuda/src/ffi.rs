use std::ffi::{c_int, c_uint, c_void};

pub type CuResult = c_int;
pub type CuDevice = c_int;
pub type CuContext = *mut c_void;

pub const CUDA_SUCCESS: CuResult = 0;

pub type CuInitFn = unsafe extern "C" fn(c_uint) -> CuResult;
pub type CuDeviceGetCountFn = unsafe extern "C" fn(*mut c_int) -> CuResult;
pub type CuDeviceGetFn = unsafe extern "C" fn(*mut CuDevice, c_int) -> CuResult;
pub type CuCtxPushCurrentFn = unsafe extern "C" fn(CuContext) -> CuResult;
pub type CuCtxPopCurrentFn = unsafe extern "C" fn(*mut CuContext) -> CuResult;
