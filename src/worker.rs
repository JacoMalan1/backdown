use std::sync::{Arc, Mutex};

use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::journal::Journal;

#[derive(Debug, Clone)]
pub enum WorkerMessage {
    FilesystemEvent(notify::Event),
}

impl std::fmt::Display for WorkerMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FilesystemEvent(ev) => f.write_fmt(format_args!(
                "FilesystemEvent(paths = {:?}, kind = {:?})",
                ev.paths, ev.kind
            )),
        }
    }
}

pub struct Worker {
    tx: Sender<WorkerMessage>,
    join_handle: JoinHandle<()>,
    cancel: CancellationToken,
    journal: Arc<Mutex<Journal>>,
}

impl Worker {
    pub fn new(cancel: CancellationToken, journal: Arc<Mutex<Journal>>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let join_handle =
            tokio::task::spawn(Self::worker(rx, cancel.clone(), Arc::clone(&journal)));
        Self {
            tx,
            join_handle,
            cancel,
            journal,
        }
    }

    async fn handle_message(message: WorkerMessage) {
        tracing::trace!("Start handle message");

        match message {
            WorkerMessage::FilesystemEvent(notify::Event { .. }) => {}
        }
    }

    async fn worker(
        mut event_rx: Receiver<WorkerMessage>,
        cancel: CancellationToken,
        _journal: Arc<Mutex<Journal>>,
    ) {
        let span = tracing::info_span!("worker");
        let _guard = span.enter();

        let mut cancelled = std::pin::pin!(cancel.cancelled());
        let mut handles = vec![];
        loop {
            tokio::select! {
                msg = event_rx.recv() => {
                    if let Some(msg) = msg {
                        tracing::trace!(msg = ?msg, "Worker received message");
                        let msg_span = tracing::info_span!("message_handler", msg = %msg);
                        handles.push(tokio::task::spawn(Self::handle_message(msg).instrument(msg_span)));
                    } else {
                        break;
                    }
                },
                _ = &mut cancelled => {
                    break;
                }
            }
        }

        for handle in handles {
            if let Err(err) = handle.await {
                tracing::warn!(error = %err, "Failed to join message handler");
            }
        }
    }

    pub async fn wait_for_shutdown(self) -> Result<(), tokio::task::JoinError> {
        self.join_handle.await
    }

    pub async fn send_message(&mut self, mut message: WorkerMessage) {
        while let Err(err) = self.tx.send(message).await {
            tracing::warn!("Worker thread must be dead, since channel is dead. Trying to spawn a new worker...");
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            let join_handle = tokio::task::spawn(Self::worker(
                rx,
                self.cancel.clone(),
                Arc::clone(&self.journal),
            ));
            let old_handle = std::mem::replace(&mut self.join_handle, join_handle);

            // Explicitly ignore the old join handle
            drop(old_handle);

            self.tx = tx;
            message = err.0;
        }
    }
}
