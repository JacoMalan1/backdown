#![warn(rust_2018_idioms, missing_debug_implementations, clippy::unwrap_used)]

use config::Config;
use std::{path::PathBuf, str::FromStr};
use tracing::level_filters::LevelFilter;

mod config;
mod journal;

#[cfg(debug_assertions)]
pub const LOG_LEVEL: LevelFilter = LevelFilter::TRACE;
#[cfg(not(debug_assertions))]
pub const LOG_LEVEL: LevelFilter = LevelFilter::INFO;

fn config_location() -> PathBuf {
    PathBuf::from_str("/etc/backdown/config.ron").unwrap()
}

fn main() -> Result<(), std::io::Error> {
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::TRACE)
        .init();

    let span = tracing::info_span!("main");
    let _guard = span.enter();

    let config_location = config_location();
    let config_str = std::fs::read_to_string(config_location)?;
    let config = match ron::from_str::<Config>(&config_str) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::warn!(error = %err, "Invalid configuration file, loading defaults...");
            Default::default()
        }
    };

    tracing::debug!(cfg = ?config, "Loaded config file");

    Ok(())
}
