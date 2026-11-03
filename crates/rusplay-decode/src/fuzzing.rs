use rusplay_core::{Codec, SignalledColor};

use crate::{DecodeError, nvdec::color::ColorReader};

pub struct ColorProbe(ColorReader);

impl ColorProbe {
    pub fn new(codec: Codec) -> Self {
        Self(ColorReader::new(codec))
    }

    pub fn read(&mut self, data: &[u8]) -> Result<SignalledColor, DecodeError> {
        self.0.read(data)?;

        Ok(self.0.current())
    }
}
