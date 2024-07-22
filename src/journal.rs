#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct Journal {
    entries: Vec<JournalEntry>,
}

impl Journal {
    pub fn new() -> Self {
        Self { entries: vec![] }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JournalEntry {
    path: std::path::PathBuf,
    last_modified: u64,
}

impl JournalEntry {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            last_modified: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Failed to get current Unix time")
                .as_secs(),
        }
    }
}
