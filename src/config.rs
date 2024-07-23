use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Default)]
#[allow(unused)]
pub struct Config {
    #[serde(default = "Vec::default")]
    pub watch_paths: Vec<WatchPath>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WatchPath {
    path: PathBuf,
}

impl WatchPath {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}
