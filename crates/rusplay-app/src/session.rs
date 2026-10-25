use rusplay_client::{ClientError, ClientSession};
use rusplay_config::{Config, ConfigEnvironment, ConfigError, ConfigRole};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Client(#[from] ClientError),
}

pub fn load_client_config(environment: &ConfigEnvironment) -> Result<Config, ConfigError> {
    let path = environment.config_path(ConfigRole::Client)?;

    Config::load_or_default(&path)
}

pub fn run_client(environment: &ConfigEnvironment) -> Result<(), SessionError> {
    let config = load_client_config(environment)?;

    ClientSession::new(config).run()?;

    Ok(())
}
