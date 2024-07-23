#![warn(rust_2018_idioms, missing_debug_implementations, clippy::unwrap_used)]

use crate::journal::Journal;
use crate::worker::{Worker, WorkerMessage};
use config::Config;
use notify::{RecursiveMode, Watcher};
use std::{
    error::Error,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
use tracing::level_filters::LevelFilter;

mod config;
mod journal;
mod worker;

#[cfg(debug_assertions)]
pub const LOG_LEVEL: LevelFilter = LevelFilter::TRACE;
#[cfg(not(debug_assertions))]
pub const LOG_LEVEL: LevelFilter = LevelFilter::INFO;

#[cfg(debug_assertions)]
fn config_location() -> PathBuf {
    PathBuf::from_str("./config.ron").unwrap()
}

#[cfg(not(debug_assertions))]
fn config_location() -> PathBuf {
    let base_dir = std::env::var("XDG_CONFIG_DIR")
        .or(std::env::var("HOME").map(|s| format!("{s}/.config")))
        .unwrap_or("/etc".to_string());

    PathBuf::from_str(&format!("{base_dir}/backdown/config.ron"))
        .expect("Failed to compute config file location")
}

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::TRACE)
        .init();

    let span = tracing::info_span!("main");
    let _guard = span.enter();

    let config_location = config_location();
    tracing::debug!("Config file location: {config_location:?}");
    let config = match tokio::fs::read_to_string(config_location)
        .await
        .map_err(|err| -> Box<dyn Error> { Box::new(err) })
        .and_then(|s| {
            ron::from_str::<Config>(&s).map_err(|err| -> Box<dyn Error> { Box::new(err) })
        }) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::error!(error = %err, "Invalid configuration file, loading defaults...");
            Default::default()
        }
    };

    tracing::debug!(cfg = ?config, "Loaded config file");

    let cancel = CancellationToken::new();
    let journal = Arc::new(Mutex::new(Journal::new()));

    let mut worker = Worker::new(cancel.clone(), journal);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);

    let mut watcher = notify::recommended_watcher(move |res| match res {
        Ok(event) => {
            tracing::info!(ev = ?event, "Filesystem event");
            tx.blocking_send(WorkerMessage::FilesystemEvent(event))
                .expect("Failed to send message to actor thread");
        }
        Err(err) => tracing::error!(error = %err, "Watch error"),
    })
    .expect("Failed to set up filesystem watcher");

    config.watch_paths.iter().for_each(|path| {
        watcher
            .watch(path.path(), RecursiveMode::Recursive)
            .expect("Failed to watch path");
    });

    loop {
        tokio::select! {
            msg = rx.recv() => {
                if let Some(msg) = msg {
                    worker.send_message(msg).await;
                } else {
                    break
                }
            },
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }

    tracing::info!("Starting graceful shutdown...");
    cancel.cancel();
    if let Err(err) = worker.wait_for_shutdown().await {
        tracing::warn!(error = %err, "Worker task joined with error");
    }

    Ok(())
}
