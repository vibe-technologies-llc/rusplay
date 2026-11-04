use std::{fmt, sync::Arc};

use rusplay_core::{FrameRate, Resolution};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRequest {
    pub resolution: Resolution,
    pub frame_rate: FrameRate,
}

#[derive(Clone)]
pub struct OutputOffer(Arc<dyn Fn(OutputRequest) + Send + Sync>);

impl OutputOffer {
    pub fn new(offer: impl Fn(OutputRequest) + Send + Sync + 'static) -> Self {
        Self(Arc::new(offer))
    }

    pub fn offer(&self, request: OutputRequest) {
        (self.0)(request);
    }
}

impl fmt::Debug for OutputOffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputOffer")
            .finish_non_exhaustive()
    }
}
