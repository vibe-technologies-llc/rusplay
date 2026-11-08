use rusplay_config::VideoDecoderConfig;
use rusplay_core::FormatSet;

use crate::ClientError;

pub fn decoder_formats(config: &VideoDecoderConfig) -> Result<FormatSet, ClientError> {
    Ok(rusplay_decode::capabilities(config)?.formats())
}

pub fn resolve_decoder(config: &VideoDecoderConfig) -> VideoDecoderConfig {
    rusplay_decode::resolve(config, None)
}
