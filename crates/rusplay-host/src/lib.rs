use rusplay_audio::AudioError;
use rusplay_capture::CaptureError;
use rusplay_config::Config;
use rusplay_encode::EncodeError;
use rusplay_input::InputError;
use rusplay_net::NetError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Net(#[from] NetError),
}

#[derive(Debug)]
pub struct HostSession {
    config: Config,
}

impl HostSession {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn run(&mut self) -> Result<(), HostError> {
        let _encoder = rusplay_encode::open(&self.config.video)?;
        let _audio = rusplay_audio::OpusAudioEncoder::new(&self.config.audio)?;

        Ok(())
    }
}
