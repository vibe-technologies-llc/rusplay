use std::ffi::CString;

use crate::{Backend, EncodeError};

pub const VBV_BUFFER_FRAMES: u64 = 2;

pub struct CParam {
    pub key: CString,
    pub value: CString,
}

impl CParam {
    pub fn new(backend: Backend, key: &str, value: &str) -> Result<Self, EncodeError> {
        let invalid = || EncodeError::InvalidParameterValue {
            backend,
            key: key.to_owned(),
            value: value.to_owned(),
        };

        Ok(Self {
            key: CString::new(key).map_err(|_| invalid())?,
            value: CString::new(value).map_err(|_| invalid())?,
        })
    }
}
