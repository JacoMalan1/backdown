use self::error::JournalError;
use crate::config::WatchPath;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::SeekFrom,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub mod error;

const DEFAULT_MAX_DIRTY_COUNT: usize = 32;

#[derive(Debug)]
pub struct Journal {
    entries: HashMap<std::path::PathBuf, JournalEntry>,
    dirty_count: usize,
    file: tokio::fs::File,
    max_dirty_count: usize,
    base_path: PathBuf,
}

impl Journal {
    pub async fn new<P>(path: P) -> Result<Self, JournalError>
    where
        P: AsRef<Path>,
    {
        Self::with_max_dirty_count(path, DEFAULT_MAX_DIRTY_COUNT).await
    }

    pub async fn with_max_dirty_count<P>(
        path: P,
        max_dirty_count: usize,
    ) -> Result<Self, JournalError>
    where
        P: AsRef<Path>,
    {
        let mut file = if !path.as_ref().exists() {
            tracing::trace!("Journal file doesn't exist. Creating it...");
            let mut file = tokio::fs::File::options()
                .write(true)
                .read(true)
                .create(true)
                .truncate(true)
                .open(path.as_ref())
                .await?;
            let buf = serde_json::to_vec::<HashMap<std::path::PathBuf, JournalEntry>>(
                &Default::default(),
            )?;
            file.write_all(&buf).await?;
            file.seek(SeekFrom::Start(0)).await?;
            file
        } else {
            tracing::trace!("Journal file found. Opening it...");
            tokio::fs::File::options()
                .write(true)
                .read(true)
                .open(path.as_ref())
                .await?
        };

        let mut base_path = path.as_ref().to_owned();
        if !base_path.pop() {
            return Err(JournalError::InvalidPath("Path has no parent".to_string()));
        }

        let mut buf = String::new();
        tracing::trace!("Reading journal file...");
        file.read_to_string(&mut buf).await?;

        tracing::trace!("Successfully read journal file.");
        Ok(Self {
            entries: serde_json::from_str(&buf)?,
            file,
            base_path,
            max_dirty_count,
            dirty_count: 0,
        })
    }

    pub async fn create_entry<P>(
        &mut self,
        watch_path: &WatchPath,
        path: P,
        backup_path: P,
    ) -> Result<&JournalEntry, JournalError>
    where
        P: AsRef<Path>,
    {
        tracing::trace!("Creating new journal entry.");

        let file = tokio::fs::File::open(&path).await?;
        let meta = file.metadata().await?;

        let entry = JournalEntry {
            file_path: path.as_ref().to_owned(),
            backup_path: backup_path.as_ref().to_owned(),
            last_modified: meta.modified()?.duration_since(UNIX_EPOCH)?.as_secs(),
            watch_path: watch_path.path().to_owned(),
            old_versions: vec![],
            deleted: None,
        };

        self.entries.insert(entry.file_path.clone(), entry);
        self.dirty_count += 1;

        if self.dirty_count >= self.max_dirty_count {
            self.flush().await?;
        }

        Ok(&self.entries[path.as_ref()])
    }

    pub async fn flush(&mut self) -> Result<usize, JournalError> {
        if self.dirty_count > 0 {
            tracing::trace!("Truncating journal file...");
            self.file.set_len(0).await?;
            self.file.seek(SeekFrom::Start(0)).await?;

            let new_contents = serde_json::to_vec(&self.entries)?;

            tracing::trace!("Writing new journal...");
            self.file.write_all(&new_contents).await?;

            Ok(std::mem::replace(&mut self.dirty_count, 0))
        } else {
            Ok(0)
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_count != 0
    }

    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    pub fn find_entry<P>(&self, path: &P) -> Option<&JournalEntry>
    where
        P: AsRef<Path>,
    {
        self.entries.get(path.as_ref())
    }

    pub fn find_entry_mut<P>(&mut self, path: &P) -> Option<EntryGuard<'_>>
    where
        P: AsRef<Path>,
    {
        self.entries.get_mut(path.as_ref()).map(|val| EntryGuard {
            entry: val,
            dirty_count: &mut self.dirty_count,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub file_path: std::path::PathBuf,
    pub watch_path: std::path::PathBuf,
    pub backup_path: std::path::PathBuf,
    pub last_modified: u64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<u64>,

    #[serde(default = "Vec::default")]
    pub old_versions: Vec<OldVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OldVersion {
    pub timestamp: u64,
    pub file_path: std::path::PathBuf,
}

impl PartialOrd for OldVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OldVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.timestamp.cmp(&other.timestamp)
    }
}

pub struct EntryGuard<'a> {
    entry: &'a mut JournalEntry,
    dirty_count: &'a mut usize,
}

impl Deref for EntryGuard<'_> {
    type Target = JournalEntry;

    fn deref(&self) -> &Self::Target {
        &*self.entry
    }
}

impl DerefMut for EntryGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.entry
    }
}

impl Drop for EntryGuard<'_> {
    fn drop(&mut self) {
        *self.dirty_count += 1;
    }
}
