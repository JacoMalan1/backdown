use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use notify::{
    event::{AccessKind, AccessMode, CreateKind},
    EventKind,
};
use thiserror::Error;
use tokio::sync::RwLock;

use crate::{
    journal::Journal,
    worker::intent::{IntentKind, WorkerIntent},
};

use super::{
    file::FileHandler,
    intent::{IntentHandler, IntentList},
};

#[derive(Debug, Clone)]
pub enum WorkerMessage {
    FilesystemEvent(notify::Event),
}

#[derive(Debug, Error)]
pub enum HandleMessageError {
    #[error("Error handling worker intent: {0}")]
    HandleIntent(#[from] super::intent::HandleIntentError),
}

#[derive(Debug)]
pub struct MessageHandler {
    journal: Arc<RwLock<Journal>>,
    intent_list: IntentList,
    file_handler: FileHandler,
    intent_handler: IntentHandler,
}

impl MessageHandler {
    pub fn new(
        journal: Arc<RwLock<Journal>>,
        intent_list: IntentList,
        intent_handler: IntentHandler,
        file_handler: FileHandler,
    ) -> Self {
        Self {
            journal,
            intent_list,
            intent_handler,
            file_handler,
        }
    }

    pub async fn handle(&self, message: WorkerMessage) -> Result<(), HandleMessageError> {
        tracing::trace!("Start handle message.");

        match message {
            WorkerMessage::FilesystemEvent(notify::Event { paths, kind, .. }) => match kind {
                EventKind::Create(CreateKind::File) => {
                    let journal = self.journal.read().await;

                    for file_path in &paths {
                        if journal.find_entry(file_path).is_some() {
                            tracing::warn!("Got a creation event, but journal entry exists. Skipping creation step...");
                        } else if !self.intent_list.has_intent_for(file_path) {
                            tracing::trace!("Creating new file creation intent.");
                            self.intent_list.create(WorkerIntent {
                                kind: IntentKind::Create,
                                path: file_path.to_owned(),
                                timestamp: SystemTime::now(),
                            })
                        }
                    }
                }
                EventKind::Access(AccessKind::Open(AccessMode::Write)) => {
                    for file_path in &paths {
                        self.intent_list.create(WorkerIntent {
                            kind: IntentKind::Modify,
                            path: file_path.to_owned(),
                            timestamp: SystemTime::now(),
                        });
                    }
                }
                EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                    for file_path in &paths {
                        match self.intent_list.remove(file_path) {
                            Some(intent) => self.intent_handler.handle(&file_path, intent).await?,
                            None => {
                                tracing::warn!(
                                    "Got file close event, but no intent has been registered. Waiting for it..."
                                );
                                let intent_list_clone = self.intent_list.clone();
                                let file_path = file_path.to_owned();
                                let intent_handler = self.intent_handler.clone();
                                tokio::spawn(async move {
                                    let intent_list = intent_list_clone;
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
                                    intent_handler.handle(file_path, intent).await
                                });
                            }
                        }
                    }
                }
                EventKind::Modify(_) => {
                    for file_path in &paths {
                        if !self.intent_list.has_intent_for(file_path) {
                            tracing::info!(file_path = ?file_path, "Received file modification event, creating intent...");
                            self.intent_list.create(WorkerIntent {
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
        Ok(())
    }
}

impl Clone for MessageHandler {
    fn clone(&self) -> Self {
        Self {
            journal: Arc::clone(&self.journal),
            intent_list: self.intent_list.clone(),
            file_handler: self.file_handler.clone(),
            intent_handler: self.intent_handler.clone(),
        }
    }
}
