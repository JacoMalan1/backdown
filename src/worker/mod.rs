use self::{error::RescanError, intent::IntentList};
use crate::{
    journal::Journal,
    worker::intent::{IntentKind, WorkerIntent},
    Config,
};
use error::WorkerError;
use intent::IntentHandler;
use message::MessageHandler;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
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
pub mod file;
pub mod intent;
pub mod message;

pub use message::WorkerMessage;

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
    pub fn new(
        cancel: CancellationToken,
        intent_handler: IntentHandler,
        message_handler: MessageHandler,
    ) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let queue = IntentList::new();
        let join_handle = tokio::task::spawn(Self::worker(
            rx,
            cancel.clone(),
            queue,
            intent_handler,
            message_handler,
        ));

        Self { tx, join_handle }
    }

    async fn worker(
        mut event_rx: Receiver<WorkerMessage>,
        cancel: CancellationToken,
        intent_list: IntentList,
        intent_handler: IntentHandler,
        message_handler: MessageHandler,
    ) {
        let mut cancelled = std::pin::pin!(cancel.cancelled());
        let mut handles = vec![];
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                msg = event_rx.recv() => {
                    if let Some(msg) = msg {
                        tracing::trace!(msg = ?msg, "Worker received message.");
                        let msg_span = tracing::info_span!("message_handler", msg = %msg);
                        let message_handler = message_handler.clone();
                        handles.push(
                            tokio::task::spawn(async move {
                                let _ = message_handler.handle(
                                    msg,
                                ).instrument(msg_span).await;
                            })
                        );
                    } else {
                        break;
                    }
                },
                _ = interval.tick() => {
                    if !intent_list.is_empty() {
                        tracing::warn!(
                            stale_intent_count = intent_list.len(),
                            "A status tick has happened and the intent list is not empty."
                        );

                        // TODO: This code causes a deadlock
                        let stale = intent_list.remove_stale(Duration::from_secs(20));
                        for intent in stale {
                            tracing::trace!(intent = ?intent, "Handling stale intent.");
                            let file_path = intent.path.to_owned();
                            let intent_handler = intent_handler.clone();
                            handles.push(
                                tokio::spawn(async move {
                                    let _ = intent_handler.handle(
                                        file_path,
                                        intent,
                                    ).await;
                                })
                            );
                        }
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

    #[async_recursion::async_recursion]
    pub async fn enumerate_files(
        path: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, tokio::io::Error> {
        if path.is_file() {
            Ok(vec![path.to_owned()])
        } else {
            let mut files = Vec::new();
            let mut read_dir = tokio::fs::read_dir(path).await?;
            while let Some(entry) = read_dir.next_entry().await? {
                let file_type = entry.file_type().await?;
                if file_type.is_file() {
                    files.push(entry.path());
                } else if file_type.is_dir() {
                    let recursive_files = Self::enumerate_files(&entry.path()).await?;
                    files.extend(recursive_files);
                }
            }
            Ok(files)
        }
    }

    pub async fn rescan(
        config: Config,
        journal: Arc<RwLock<Journal>>,
        intent_handler: IntentHandler,
    ) -> Result<(), RescanError> {
        let mut modified_files = vec![];
        let mut new_files = vec![];

        let journal_lck = journal.read().await;

        for watch_path in &config.watch_paths {
            let path = watch_path.path();
            let files = Self::enumerate_files(path).await?;
            for path in files {
                let meta = path.metadata()?;
                if let Some(entry) = journal_lck.find_entry(&path) {
                    let timestamp = meta.modified()?.duration_since(UNIX_EPOCH)?.as_secs();
                    if timestamp > entry.last_modified {
                        modified_files.push(path);
                    }
                } else {
                    new_files.push(path);
                }
            }
        }

        drop(journal_lck);

        for new_file in new_files {
            if let Err(err) = intent_handler
                .handle(
                    new_file.clone(),
                    WorkerIntent {
                        path: new_file,
                        kind: IntentKind::Create,
                        timestamp: SystemTime::now(),
                    },
                )
                .await
            {
                tracing::error!(error = %err, "Failed to handle file creation intent");
            }
        }

        for modified in modified_files {
            if let Err(err) = intent_handler
                .handle(
                    modified.clone(),
                    WorkerIntent {
                        path: modified,
                        kind: IntentKind::Modify,
                        timestamp: SystemTime::now(),
                    },
                )
                .await
            {
                tracing::error!(error = %err, "Failed to handle file modification intent");
            }
        }

        Ok(())
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
