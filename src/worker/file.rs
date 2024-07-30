use crate::config::Config;
use crate::journal::{Journal, OldVersion};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::sync::RwLock;

#[derive(Debug, Error)]
pub enum HandleFileError {
    #[error("I/O Error: {0}")]
    IO(#[from] std::io::Error),

    #[error("No such watch path")]
    MissingWatchPath,

    #[error("Journal error: {0}")]
    JournalError(#[from] crate::journal::error::JournalError),

    #[error("Invalid file path")]
    InvalidFilePath,

    #[error("System time error: {0}")]
    SystemTime(#[from] std::time::SystemTimeError),
}

#[derive(Debug)]
pub struct FileHandler {
    config: Config,
    journal: Arc<RwLock<Journal>>,
}

impl FileHandler {
    pub fn new(config: Config, journal: Arc<RwLock<Journal>>) -> Self {
        Self { config, journal }
    }

    pub async fn create<P>(&self, file_path: P) -> Result<(), HandleFileError>
    where
        P: AsRef<std::path::Path>,
    {
        let file_path = file_path.as_ref();
        let mut journal_lck = self.journal.write().await;

        let entry = journal_lck.find_entry(&file_path);
        let base_path = journal_lck.base_path().to_owned();

        let entry = if let Some(entry) = entry {
            entry
        } else {
            let watch_path = self
                .config
                .find_watch_path(file_path)
                .ok_or(HandleFileError::MissingWatchPath)?;

            let relative_path = file_path
                .strip_prefix(watch_path.path())
                .map_err(|_| HandleFileError::InvalidFilePath)?;

            let mut backup_path = base_path.join(relative_path);
            let file_name = backup_path
                .file_name()
                .ok_or(HandleFileError::InvalidFilePath)?
                .to_string_lossy()
                .into_owned();

            let mut counter = 0;
            while backup_path.exists() {
                let file_name = format!("{}.{counter}", file_name);
                backup_path.set_file_name(file_name);
                counter += 1;
            }

            journal_lck
                .create_entry(
                    self.config
                        .find_watch_path(file_path)
                        .ok_or(HandleFileError::MissingWatchPath)?,
                    file_path,
                    &backup_path,
                )
                .await?
        };

        if let Some(parent) = &entry.backup_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::copy(file_path, &entry.backup_path).await?;

        Ok(())
    }

    pub async fn modify<P>(&self, file_path: P) -> Result<(), HandleFileError>
    where
        P: AsRef<std::path::Path>,
    {
        let file_path = file_path.as_ref();
        let mut journal_lck = self.journal.write().await;

        let entry_guard = journal_lck.find_entry_mut(&file_path);
        if let Some(mut entry) = entry_guard {
            let mut file = tokio::fs::File::open(&file_path).await?;

            let metadata = file.metadata().await?;

            let modification_time = metadata
                .modified()
                .unwrap_or(SystemTime::now())
                .duration_since(UNIX_EPOCH)?
                .as_secs();

            let backup_path = entry.backup_path.clone();

            if backup_path.exists() {
                // TODO: For large files, this method of computing their hashes will likely cause
                // an OOM error.

                let mut buffer = vec![0; metadata.size() as usize];
                file.read_to_end(&mut buffer).await?;

                let file_hash = tokio::task::spawn_blocking(move || sha256::digest(buffer))
                    .await
                    .expect("Failed to join digest compute blocking task");

                let mut backup_file = tokio::fs::File::open(&backup_path).await?;

                let metadata = backup_file.metadata().await?;

                buffer = vec![0; metadata.size() as usize];
                backup_file.read_to_end(&mut buffer).await?;

                let backup_hash = tokio::task::spawn_blocking(move || sha256::digest(buffer))
                    .await
                    .expect("Failed to join digest compute blocking task");

                if file_hash == backup_hash {
                    tracing::warn!("Received modification event, but the file is identical to it's backup. Ignoring...");
                    return Ok(());
                }
            }

            let old_timestamp = std::mem::replace(&mut entry.last_modified, modification_time);

            let mut old_version_path = backup_path.clone();

            old_version_path.set_file_name(format!(
                "{}.{old_timestamp}",
                old_version_path
                    .file_name()
                    .ok_or(HandleFileError::InvalidFilePath)?
                    .to_string_lossy()
            ));

            if backup_path.exists() {
                tracing::info!(old_path = ?backup_path, new_path = ?old_version_path, "Renaming old file.");
                tokio::fs::rename(&backup_path, &old_version_path).await?;
            }

            entry.old_versions.push(OldVersion {
                timestamp: old_timestamp,
                file_path: old_version_path,
            });

            tokio::fs::copy(&file_path, &backup_path).await?;

            Ok(())
        } else {
            tracing::warn!(
                file_path = ?file_path,
                "Modification event fired on file not in journal. Handling creation instead..."
            );

            drop(entry_guard);
            drop(journal_lck);

            self.create(file_path).await
        }
    }

    pub async fn remove<P>(&self, file_path: P) -> Result<(), HandleFileError>
    where
        P: AsRef<std::path::Path>,
    {
        let mut journal = self.journal.write().await;
        let entry_guard = journal.find_entry_mut(&file_path);
        if let Some(mut entry) = entry_guard {
            entry.deleted = Some(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
            Ok(())
        } else {
            tracing::warn!(file_path = ?file_path.as_ref(), "Remove event fired on file not in journal. Ignoring it ...");
            Ok(())
        }
    }
}

impl Clone for FileHandler {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            journal: Arc::clone(&self.journal),
        }
    }
}
