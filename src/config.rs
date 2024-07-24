use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[allow(unused)]
pub struct Config {
    #[serde(default = "Vec::default")]
    pub watch_paths: Vec<WatchPath>,
    pub backup_path: PathBuf,
    #[serde(default = "Config::default_fs_refresh_timeout_secs")]
    pub fs_refresh_timeout_secs: u64,
}

impl Config {
    pub fn default_fs_refresh_timeout_secs() -> u64 {
        60
    }

    pub fn find_watch_path<P>(&self, needle: P) -> Option<&WatchPath>
    where
        P: AsRef<Path>,
    {
        self.watch_paths
            .iter()
            .find(|path| needle.as_ref().starts_with(&path.path))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchPath {
    path: PathBuf,
    #[serde(default = "Vec::default")]
    ignore_patterns: Vec<String>,
}

impl WatchPath {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}
