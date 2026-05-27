use anyhow::Result;
use clap::Parser;

use aquafin::cli::{Cli, LogLevel};
use aquafin::config::Config;

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load config (best-effort) so its log settings can apply at startup.
    let config = Config::load().ok().flatten();
    let level = cli
        .log_level
        .or_else(|| config.as_ref().and_then(|c| c.log.level))
        .unwrap_or(LogLevel::Info)
        .as_level_filter();
    let max_files = config.as_ref().and_then(|c| c.log.max_files).unwrap_or(5);

    // Hold the guard for the program's lifetime so logs flush on exit.
    let _log_guard = aquafin::error::init_logging(level, max_files)?;
    aquafin::error::install_panic_hook();

    tracing::info!(setup = cli.setup, "starting aquafin");
    aquafin::ui::run(cli.setup)
}
