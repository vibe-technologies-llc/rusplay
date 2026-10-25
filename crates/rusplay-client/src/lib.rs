use rusplay_audio::AudioError;
use rusplay_config::Config;
use rusplay_decode::DecodeError;
use rusplay_input::InputError;
use rusplay_net::NetError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Net(#[from] NetError),
}

#[derive(Debug)]
pub struct ClientSession {
    config: Config,
}

impl ClientSession {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn run(&mut self) -> Result<(), ClientError> {
        let _decoder = rusplay_decode::open(self.config.video.codec, &self.config.video.decoder)?;
        let _audio = rusplay_audio::OpusAudioDecoder::new(&self.config.audio)?;

        Ok(())
    }
}
