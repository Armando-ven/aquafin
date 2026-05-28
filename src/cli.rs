use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use tracing::level_filters::LevelFilter;

/// Command-line arguments for aquafin.
#[derive(Debug, Parser)]
#[command(name = "aquafin", version, about = "Jellyfin TUI client for the terminal.")]
pub struct Cli {
    /// Re-run the first-launch setup wizard, overwriting any existing config.
    #[arg(long)]
    pub setup: bool,

    /// Logging verbosity (overrides the `log.level` config field).
    #[arg(long, value_enum)]
    pub log_level: Option<LogLevel>,

    /// Import a theme TOML into `$XDG_CONFIG_HOME/aquafin/themes/`, then exit.
    #[arg(long, value_name = "PATH")]
    pub import_theme: Option<std::path::PathBuf>,
}

/// Logging verbosity, mirroring `tracing`'s levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_level_filter(self) -> LevelFilter {
        match self {
            LogLevel::Error => LevelFilter::ERROR,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Trace => LevelFilter::TRACE,
        }
    }
}
