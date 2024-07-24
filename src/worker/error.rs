use thiserror::Error;

#[derive(Debug, Clone, Error)]
#[error("Worker error")]
pub enum WorkerError {
    #[error("Worker channel is closed. Worker is dead.")]
    ChannelClosed(super::WorkerMessage),
}
