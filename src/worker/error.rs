use thiserror::Error;

#[derive(Debug, Clone, Error)]
#[error("Worker error")]
pub enum WorkerError {
    #[error("Worker channel is closed. Worker is dead.")]
    ChannelClosed(super::WorkerMessage),
}

#[derive(Debug, Error)]
pub enum RescanError {
    #[error("I/O error while rescanning filesystem: {0}")]
    IO(#[from] std::io::Error),

    #[error("SystemTime error while rescanning filesystem: {0}")]
    SystemTime(#[from] std::time::SystemTimeError),
}
