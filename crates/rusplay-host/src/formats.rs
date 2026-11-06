use rusplay_config::VideoEncoderConfig;
use rusplay_core::FormatSet;

use crate::HostError;

pub fn encoder_formats(config: &VideoEncoderConfig) -> Result<FormatSet, HostError> {
    Ok(rusplay_encode::capabilities(config)?.format_set())
}
