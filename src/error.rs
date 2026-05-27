//! Logging setup and the terminal-restoring panic hook.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};

/// Initialize file logging into `$XDG_STATE_HOME/aquafin/` with daily rotation,
/// keeping the last `max_files` files. The returned [`WorkerGuard`] must be held
/// for the lifetime of the program (drop flushes the non-blocking writer).
pub fn init_logging(level: LevelFilter, max_files: usize) -> Result<WorkerGuard> {
    let dir = crate::paths::state_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating log dir {}", dir.display()))?;
    let appender = build_file_appender(&dir, max_files)?;
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_max_level(level)
        .with_target(true)
        .init();
    Ok(guard)
}

fn build_file_appender(dir: &Path, max_files: usize) -> Result<RollingFileAppender> {
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("aquafin")
        .filename_suffix("log")
        .max_log_files(max_files.max(1))
        .build(dir)
        .context("building rolling log file appender")
}

/// Install a panic hook that logs the panic, writes a crash file, restores the
/// terminal, prints a pointer to the log, and exits non-zero.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let payload = format!("{info}\n\nBacktrace:\n{backtrace}");

        tracing::error!("panic: {payload}"); // best-effort to the rolling log
        let crash_path = write_crash_log(&payload); // guaranteed synchronous record

        force_restore_terminal();

        let location = crash_path
            .map(|p| p.display().to_string())
            .or_else(|| crate::paths::state_dir().ok().map(|p| p.display().to_string()))
            .unwrap_or_else(|| "<unknown>".to_string());
        eprintln!("aquafin crashed. Log: {location}");
        std::process::exit(1);
    }));
}

fn write_crash_log(payload: &str) -> Option<PathBuf> {
    let dir = crate::paths::state_dir().ok()?;
    write_crash_log_to(&dir, payload).ok()
}

fn write_crash_log_to(dir: &Path, payload: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("aquafin-crash.log");
    std::fs::write(&path, format!("=== aquafin panic ===\n{payload}\n"))?;
    Ok(path)
}

/// Leave the alternate screen and disable raw mode without needing the `Terminal`
/// handle — used from the panic hook.
fn force_restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn file_appender_writes_a_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut appender = build_file_appender(dir.path(), 5).unwrap();
        writeln!(appender, "hello log").unwrap();
        appender.flush().unwrap();

        let has_log = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with("aquafin"));
        assert!(has_log, "expected an aquafin*.log file to be created");
    }

    #[test]
    fn crash_log_contains_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_crash_log_to(dir.path(), "boom at line 42").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("boom at line 42"));
        assert!(content.contains("panic"));
    }
}
