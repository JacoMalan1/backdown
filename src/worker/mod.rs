use crate::{journal::Journal, Config};
use error::WorkerError;
use notify::EventKind;
use std::{path::Path, sync::Arc};
use tokio::{
    sync::{
        mpsc::{Receiver, Sender},
        RwLock,
    },
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

pub mod error;

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
}

impl Worker {
    pub fn new<P>(
        cancel: CancellationToken,
        journal: Arc<RwLock<Journal>>,
        base_path: P,
        config: Config,
    ) -> Self
    where
        P: AsRef<Path> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let join_handle = tokio::task::spawn(Self::worker(
            rx,
            cancel.clone(),
            config,
            Arc::clone(&journal),
            base_path,
        ));

        Self { tx, join_handle }
    }

    async fn handle_message(message: WorkerMessage, config: Config, journal: Arc<RwLock<Journal>>) {
        tracing::trace!("Start handle message.");

        match message {
            WorkerMessage::FilesystemEvent(notify::Event { paths, kind, .. }) => match kind {
                EventKind::Create(_create_kind) => {
                    let watch_paths = paths
                        .iter()
                        .flat_map(|p| config.find_watch_path(p))
                        .zip(paths.iter());
                    let mut journal = journal.write().await;

                    for (watch_path, file_path) in watch_paths {
                        if journal.find_entry(file_path).is_some() {
                            tracing::warn!("Got a creation event, but journal entry exists. Skipping creation step...");
                        } else {
                            let span =
                                tracing::info_span!("create_journal_entry", path = ?file_path);

                            let base_path = journal.base_path().to_owned();
                            let new_entry = journal
                                .create_entry(watch_path, file_path)
                                .instrument(span)
                                .await;

                            match new_entry {
                                Ok(entry) => {
                                    let relative_path = entry
                                        .file_path
                                        .strip_prefix(&entry.watch_path)
                                        .expect("Failed to compute relative path");

                                    let backup_path = base_path.join(relative_path);

                                    tracing::debug!(
                                        backup_path = ?backup_path,
                                        file_path = ?&entry.file_path,
                                        watch_path = ?&entry.watch_path,
                                        relative_path = ?relative_path,
                                        "Computed file backup path."
                                    );

                                    if backup_path.exists() {
                                        panic!("Backup path already exists on disk. Refusing to overwrite...");
                                    }

                                    if let Err(err) =
                                        tokio::fs::copy(&entry.file_path, &backup_path).await
                                    {
                                        tracing::error!(error = %err, "Failed to create backup of file.");
                                    }
                                }
                                Err(err) => {
                                    tracing::error!(error = %err, "Failed to create journal entry.");
                                }
                            }
                        }
                    }
                }
                EventKind::Modify(_modify_kind) => todo!("Handle inode modification"),
                EventKind::Remove(_remove_kind) => todo!("Handle inode destruction"),
                _ => (),
            },
        }
    }

    async fn worker<P>(
        mut event_rx: Receiver<WorkerMessage>,
        cancel: CancellationToken,
        config: Config,
        journal: Arc<RwLock<Journal>>,
        _base_path: P,
    ) where
        P: AsRef<Path> + Send + 'static,
    {
        let mut cancelled = std::pin::pin!(cancel.cancelled());
        let mut handles = vec![];
        loop {
            tokio::select! {
                msg = event_rx.recv() => {
                    if let Some(msg) = msg {
                        tracing::trace!(msg = ?msg, "Worker received message.");
                        let msg_span = tracing::info_span!("message_handler", msg = %msg);
                        handles.push(tokio::task::spawn(Self::handle_message(msg, config.clone(), Arc::clone(&journal)).instrument(msg_span)));
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
                tracing::warn!(error = %err, "Failed to join message handler.");
            }
        }
    }

    pub async fn wait_for_shutdown(self) -> Result<(), tokio::task::JoinError> {
        self.join_handle.await
    }

    pub async fn send_message(&mut self, message: WorkerMessage) -> Result<(), WorkerError> {
        self.tx
            .send(message)
            .await
            .map_err(|err| WorkerError::ChannelClosed(err.0))
    }
}
