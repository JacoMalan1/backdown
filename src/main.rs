#![warn(rust_2018_idioms, missing_debug_implementations, clippy::unwrap_used)]

use crate::journal::Journal;
use crate::worker::{Worker, WorkerMessage};
use config::Config;
use notify::{RecursiveMode, Watcher};
use std::time::Duration;
use std::{error::Error, path::PathBuf, str::FromStr, sync::Arc};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::level_filters::LevelFilter;
use tracing::Instrument;

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
            panic!("Invalid configuration file: {err:?}");
        }
    };

    tracing::debug!(cfg = ?config, "Loaded config file");

    let base_path = config.backup_path.clone();
    if !base_path.is_dir() {
        tracing::error!("The backup path should be a directory, not a file");
        return Ok(());
    }

    let mut journal_path = base_path.clone();
    journal_path.push(".backdown.journal.json");

    let cancel = CancellationToken::new();
    let journal = Arc::new(RwLock::new(
        Journal::new(journal_path)
            .await
            .expect("Failed to create journal"),
    ));

    let mut worker = Worker::new(
        cancel.clone(),
        Arc::clone(&journal),
        base_path,
        config.clone(),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);

    let mut watcher = notify::recommended_watcher(move |res| match res {
        Ok(event) => {
            tracing::info!(ev = ?event, "Filesystem event.");
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

    let interval_journal = Arc::clone(&journal);
    let interval_cancel = cancel.clone();
    let interval_span = tracing::info_span!("interval_flush");
    let interval_handle = tokio::task::spawn(
        async move {
            let journal = interval_journal;
            let cancel = interval_cancel;
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        tracing::info!("Flushing journal...");
                        let mut journal = journal.write().await;
                        if let Err(err) = journal.flush().await {
                            tracing::error!(error = %err, "Failed to flush journal.");
                        }
                    },
                    _ = cancel.cancelled() => {
                        break;
                    }
                }
            }
        }
        .instrument(interval_span),
    );

    loop {
        tokio::select! {
            msg = rx.recv() => {
                if let Some(msg) = msg {
                    if let Err(err) = worker.send_message(msg).await {
                        tracing::error!(error = %err, "Failed to send message to worker.");
                        break;
                    }
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

    if let Err(err) = interval_handle.await {
        tracing::warn!(error = %err, "Journal flush task joined with error.");
    }

    if let Err(err) = worker.wait_for_shutdown().await {
        tracing::warn!(error = %err, "Worker task joined with error.");
    }

    tracing::info!("Flushing journal...");
    let flush_span = tracing::info_span!("journal_exit_flush");
    let mut journal_lck = journal.write().await;
    let flushed = journal_lck
        .flush()
        .instrument(flush_span)
        .await
        .expect("Failed to flush journal.");

    tracing::info!(entries_flushed = flushed, "Journal flushed.");

    Ok(())
}
