use thiserror::Error;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("screencast portal request failed")]
    Portal(#[from] ashpd::Error),
    #[error("the portal returned no streams")]
    NoStreams,
    #[error("the pipewire stream ended")]
    StreamEnded,
}
