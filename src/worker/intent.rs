use super::file::FileHandler;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct WorkerIntent {
    pub path: std::path::PathBuf,
    pub kind: IntentKind,
    pub timestamp: SystemTime,
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[allow(dead_code)]
pub enum IntentKind {
    Create,
    Remove,
    Modify,
}

#[derive(Debug)]
pub struct IntentList {
    map: Arc<RwLock<HashMap<std::path::PathBuf, WorkerIntent>>>,
}

impl IntentList {
    pub fn new() -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn create(&self, intent: WorkerIntent) {
        let mut map = self.map.write().unwrap_or_else(|err| err.into_inner());
        map.insert(intent.path.to_owned(), intent);
    }

    pub fn remove<P>(&self, path: P) -> Option<WorkerIntent>
    where
        P: AsRef<std::path::Path>,
    {
        let mut map = self.map.write().unwrap_or_else(|err| err.into_inner());
        map.remove(path.as_ref())
    }

    pub fn has_intent_for<P>(&self, path: P) -> bool
    where
        P: AsRef<std::path::Path>,
    {
        let map = self.map.read().unwrap_or_else(|err| err.into_inner());
        map.contains_key(path.as_ref())
    }

    pub fn len(&self) -> usize {
        let lck = self.map.read().unwrap_or_else(|err| err.into_inner());
        lck.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn remove_stale(&self, max_age: Duration) -> Vec<WorkerIntent> {
        let mut map = self.map.write().unwrap_or_else(|err| err.into_inner());
        let mut to_remove = vec![];
        map.iter().for_each(|(key, value)| {
            if SystemTime::now()
                .duration_since(value.timestamp)
                .expect("Failed to calculate duration since Intent creation")
                >= max_age
            {
                to_remove.push(key.to_owned());
            }
        });

        to_remove
            .into_iter()
            .flat_map(|key| map.remove(&key))
            .collect::<Vec<_>>()
    }
}

impl Clone for IntentList {
    fn clone(&self) -> Self {
        Self {
            map: Arc::clone(&self.map),
        }
    }
}

#[derive(Debug, Error)]
pub enum HandleIntentError {
    #[error("Error while handling file event: {0}")]
    HandleFile(#[from] super::file::HandleFileError),
}

#[derive(Debug, Clone)]
pub struct IntentHandler {
    file_handler: FileHandler,
}

impl IntentHandler {
    pub fn new(file_handler: FileHandler) -> Self {
        Self { file_handler }
    }

    pub async fn handle<P>(
        &self,
        file_path: P,
        intent: WorkerIntent,
    ) -> Result<(), HandleIntentError>
    where
        P: AsRef<std::path::Path>,
    {
        match intent {
            WorkerIntent {
                kind: IntentKind::Modify,
                ..
            } => self.file_handler.modify(&file_path).await?,
            WorkerIntent {
                kind: IntentKind::Create,
                ..
            } => self.file_handler.create(&file_path).await?,
            _ => (),
        }
        Ok(())
    }
}
