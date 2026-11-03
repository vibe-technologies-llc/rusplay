use std::fmt;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CudaError {
    #[error("could not load {library}")]
    LibraryNotFound {
        library: &'static str,
        #[source]
        source: libloading::Error,
    },
    #[error("{library} has no symbol {symbol}")]
    SymbolMissing {
        library: &'static str,
        symbol: &'static str,
        #[source]
        source: libloading::Error,
    },
    #[error("cuda device {index} does not exist, the driver reports {count} devices")]
    DeviceMissing { index: u32, count: u32 },
    #[error("{call} failed with {status}")]
    Call { call: CudaCall, status: CudaStatus },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CudaCall {
    Init,
    DeviceGetCount,
    DeviceGet,
    DevicePrimaryCtxRetain,
    CtxCreate,
    CtxPushCurrent,
    CtxPopCurrent,
    ImportExternalMemory,
    ExternalMemoryGetMappedBuffer,
    Memcpy2D,
}

impl fmt::Display for CudaCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Init => "cuInit",
            Self::DeviceGetCount => "cuDeviceGetCount",
            Self::DeviceGet => "cuDeviceGet",
            Self::DevicePrimaryCtxRetain => "cuDevicePrimaryCtxRetain",
            Self::CtxCreate => "cuCtxCreate",
            Self::CtxPushCurrent => "cuCtxPushCurrent",
            Self::CtxPopCurrent => "cuCtxPopCurrent",
            Self::ImportExternalMemory => "cuImportExternalMemory",
            Self::ExternalMemoryGetMappedBuffer => "cuExternalMemoryGetMappedBuffer",
            Self::Memcpy2D => "cuMemcpy2D",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CudaStatus(pub i32);

impl CudaStatus {
    const NAMES: [(i32, &'static str); 22] = [
        (1, "invalid value"),
        (2, "out of memory"),
        (3, "not initialized"),
        (4, "deinitialized"),
        (34, "stub library"),
        (35, "insufficient driver"),
        (46, "device unavailable"),
        (100, "no device"),
        (101, "invalid device"),
        (200, "invalid image"),
        (201, "invalid context"),
        (205, "map failed"),
        (206, "unmap failed"),
        (216, "context already in use"),
        (304, "operating system call failed"),
        (400, "invalid handle"),
        (500, "not found"),
        (600, "not ready"),
        (700, "illegal address"),
        (801, "not supported"),
        (802, "system not ready"),
        (999, "unknown error"),
    ];
}

impl fmt::Display for CudaStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match Self::NAMES.iter().find(|(code, _)| *code == self.0) {
            Some((_, name)) => write!(f, "{name} ({})", self.0),
            None => write!(f, "cuda error {}", self.0),
        }
    }
}
