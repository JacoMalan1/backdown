use self::intent::IntentList;
use crate::{
    journal::{Journal, OldVersion},
    worker::intent::{IntentKind, WorkerIntent},
    Config,
};
use error::WorkerError;
use notify::{
    event::{AccessKind, AccessMode, CreateKind},
    EventKind,
};
use std::{
    os::unix::fs::MetadataExt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncReadExt,
    sync::{
        mpsc::{Receiver, Sender},
        RwLock,
    },
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

pub mod error;
pub mod intent;

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
    pub fn new(cancel: CancellationToken, journal: Arc<RwLock<Journal>>, config: Config) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let queue = IntentList::new();
        let join_handle = tokio::task::spawn(Self::worker(
            rx,
            cancel.clone(),
            config,
            Arc::clone(&journal),
            queue,
        ));

        Self { tx, join_handle }
    }

    async fn handle_file_create(
        config: Config,
        journal: Arc<RwLock<Journal>>,
        file_path: std::path::PathBuf,
    ) {
        let mut journal_lck = journal.write().await;
        let entry = journal_lck.find_entry(&file_path);
        let base_path = journal_lck.base_path().to_owned();

        let entry = if let Some(entry) = entry {
            entry
        } else {
            let new_entry = journal_lck
                .create_entry(
                    config
                        .find_watch_path(&file_path)
                        .expect("No such watch path"),
                    file_path.clone(),
                )
                .await;
            new_entry.expect("Failed to create new journal entry")
        };

        let relative_path = file_path
            .strip_prefix(&entry.watch_path)
            .expect("Invalid file path");

        let backup_path = base_path.join(relative_path);

        if let Some(parent) = backup_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("Failed to create backup path directories");
        }

        tokio::fs::copy(file_path, backup_path)
            .await
            .expect("Failed to backup file");
    }

    async fn handle_file_update(
        config: Config,
        journal: Arc<RwLock<Journal>>,
        file_path: std::path::PathBuf,
    ) {
        let mut journal_lck = journal.write().await;

        let base_path = journal_lck.base_path().to_owned();
        let entry_guard = journal_lck.find_entry_mut(&file_path);
        if let Some(mut entry) = entry_guard {
            let mut file = tokio::fs::File::open(&file_path)
                .await
                .expect("Failed to open file");

            let metadata = file.metadata().await.expect("Failed to get file metadata");

            let modification_time = metadata
                .modified()
                .unwrap_or(SystemTime::now())
                .duration_since(UNIX_EPOCH)
                .expect("Failed to calculate modification timestamp")
                .as_secs();

            let relative_path = entry
                .file_path
                .strip_prefix(&entry.watch_path)
                .expect("Failed to compute relative path");

            let backup_path = base_path.join(relative_path);

            if backup_path.exists() {
                // TODO: For large files, this method of computing their hashes will likely cause
                // an OOM error.

                let mut buffer = vec![0; metadata.size() as usize];
                file.read_to_end(&mut buffer)
                    .await
                    .expect("Failed to read file");

                let file_hash = tokio::task::spawn_blocking(move || sha256::digest(buffer))
                    .await
                    .expect("Failed to join digest compute blocking task");

                let mut backup_file = tokio::fs::File::open(&backup_path)
                    .await
                    .expect("Failed to open backed up file");

                let metadata = backup_file
                    .metadata()
                    .await
                    .expect("Failed to get backup file metadata");

                buffer = vec![0; metadata.size() as usize];
                backup_file
                    .read_to_end(&mut buffer)
                    .await
                    .expect("Failed to read backup file");

                let backup_hash = tokio::task::spawn_blocking(move || sha256::digest(buffer))
                    .await
                    .expect("Failed to join digest compute blocking task");

                if file_hash == backup_hash {
                    tracing::warn!("Received modification event, but the file is identical to it's backup. Ignoring...");
                    return;
                }
            }

            let old_timestamp = std::mem::replace(&mut entry.last_modified, modification_time);

            let mut old_version_path = backup_path.clone();

            old_version_path.set_file_name(format!(
                "{}.{old_timestamp}",
                old_version_path
                    .file_name()
                    .expect("Invalid file name")
                    .to_string_lossy()
            ));

            tracing::info!(old_path = ?backup_path, new_path = ?old_version_path, "Renaming old file.");
            tokio::fs::rename(&backup_path, &old_version_path)
                .await
                .expect("Failed to rename old file");

            entry.old_versions.push(OldVersion {
                timestamp: old_timestamp,
                file_path: old_version_path,
            });

            tokio::fs::copy(&file_path, &backup_path)
                .await
                .expect("Failed to backup file");
        } else {
            tracing::warn!(
                file_path = ?file_path,
                "Modification event fired on file not in journal. Handling creation instead..."
            );

            drop(entry_guard);
            drop(journal_lck);

            Self::handle_file_create(config.clone(), Arc::clone(&journal), file_path).await;
        };
    }

    async fn handle_message(
        message: WorkerMessage,
        config: Config,
        journal: Arc<RwLock<Journal>>,
        mut intent_list: IntentList,
    ) {
        tracing::trace!("Start handle message.");

        match message {
            WorkerMessage::FilesystemEvent(notify::Event { paths, kind, .. }) => match kind {
                EventKind::Create(CreateKind::File) => {
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

                            let new_entry = journal
                                .create_entry(watch_path, file_path)
                                .instrument(span)
                                .await;

                            match new_entry {
                                Ok(_) => {
                                    if !intent_list.has_intent_for(file_path) {
                                        tracing::trace!("Creating new file creation intent.");
                                        intent_list.create(WorkerIntent {
                                            kind: IntentKind::Create,
                                            path: file_path.to_owned(),
                                            timestamp: SystemTime::now(),
                                        })
                                    }
                                }
                                Err(err) => {
                                    tracing::error!(error = %err, "Failed to create journal entry.");
                                }
                            }
                        }
                    }
                }
                EventKind::Access(AccessKind::Open(AccessMode::Write)) => {
                    for file_path in &paths {
                        intent_list.create(WorkerIntent {
                            kind: IntentKind::Modify,
                            path: file_path.to_owned(),
                            timestamp: SystemTime::now(),
                        });
                    }
                }
                EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                    for file_path in &paths {
                        match intent_list.remove(file_path) {
                            Some(intent) => {
                                Self::handle_intent(
                                    config.clone(),
                                    intent,
                                    file_path.to_owned(),
                                    Arc::clone(&journal),
                                )
                                .await
                            }
                            None => {
                                tracing::warn!(
                                    "Got file close event, but no intent has been registered. Waiting for it..."
                                );
                                let intent_list_clone = intent_list.clone();
                                let file_path = file_path.to_owned();
                                let journal = Arc::clone(&journal);
                                let config_cloned = config.clone();
                                tokio::spawn(async move {
                                    let mut intent_list = intent_list_clone;
                                    let mut total_wait_secs: usize = 0;
                                    let intent = loop {
                                        if total_wait_secs >= 600 {
                                            panic!("Intent creation wait timed out.");
                                        }

                                        tokio::time::sleep(Duration::from_secs(5)).await;
                                        if let Some(intent) = intent_list.remove(&file_path) {
                                            break intent;
                                        }

                                        total_wait_secs += 5;

                                        tracing::trace!(
                                            "Waited 5 seconds. Intent still missing..."
                                        );
                                    };
                                    Self::handle_intent(
                                        config_cloned.clone(),
                                        intent,
                                        file_path,
                                        Arc::clone(&journal),
                                    )
                                    .await;
                                });
                            }
                        }
                    }
                }
                EventKind::Modify(_) => {
                    for file_path in &paths {
                        if !intent_list.has_intent_for(file_path) {
                            tracing::info!(file_path = ?file_path, "Received file modification event, creating intent...");
                            intent_list.create(WorkerIntent {
                                path: file_path.to_owned(),
                                kind: IntentKind::Modify,
                                timestamp: SystemTime::now(),
                            });
                        }
                    }
                }
                EventKind::Remove(_remove_kind) => todo!("Handle inode destruction"),
                _ => (),
            },
        }
    }

    async fn handle_intent(
        config: Config,
        intent: WorkerIntent,
        file_path: std::path::PathBuf,
        journal: Arc<RwLock<Journal>>,
    ) {
        match intent {
            WorkerIntent {
                kind: IntentKind::Modify,
                ..
            } => {
                Self::handle_file_update(
                    config.clone(),
                    Arc::clone(&journal),
                    file_path.to_owned(),
                )
                .await;
            }
            WorkerIntent {
                kind: IntentKind::Create,
                ..
            } => {
                Self::handle_file_create(config.clone(), Arc::clone(&journal), file_path.to_owned())
                    .await
            }
            _ => (),
        }
    }

    async fn worker(
        mut event_rx: Receiver<WorkerMessage>,
        cancel: CancellationToken,
        config: Config,
        journal: Arc<RwLock<Journal>>,
        mut intent_list: IntentList,
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
                        handles.push(tokio::task::spawn(Self::handle_message(msg, config.clone(), Arc::clone(&journal), intent_list.clone()).instrument(msg_span)));
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
                        for intent in intent_list.remove_stale(Duration::from_secs(60)) {
                            let file_path = intent.path.to_owned();
                            Self::handle_intent(config.clone(), intent, file_path, Arc::clone(&journal)).await;
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
