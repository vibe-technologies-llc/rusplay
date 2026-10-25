use rusplay_core::{Resolution, ScaleFilter};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ScaleError {
    #[error("source resolution {0} has no pixels")]
    EmptySource(Resolution),
    #[error("target resolution {0} has no pixels")]
    EmptyTarget(Resolution),
    #[error("the polar pass for {0} is not built yet")]
    PolarPassUnavailable(ScaleFilter),
}
