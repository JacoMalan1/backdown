use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};

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

pub struct IntentList {
    map: Arc<RwLock<HashMap<std::path::PathBuf, WorkerIntent>>>,
}

impl IntentList {
    pub fn new() -> Self {
        Self {
            map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn create(&mut self, intent: WorkerIntent) {
        let mut map = self.map.write().unwrap_or_else(|err| err.into_inner());
        map.insert(intent.path.to_owned(), intent);
    }

    pub fn remove<P>(&mut self, path: P) -> Option<WorkerIntent>
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

    pub fn remove_stale(&mut self, max_age: Duration) -> Vec<WorkerIntent> {
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
