//! XDG base directory resolution for aquafin.
//!
//! Standard directories follow the XDG Base Directory Specification (honouring
//! `XDG_*` env vars with the usual `~/.config`, `~/.cache`, etc. fallbacks).

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::{BaseDirs, ProjectDirs};

const APP: &str = "aquafin";

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", APP)
        .context("could not determine XDG base directories (is $HOME set?)")
}

/// `$XDG_CONFIG_HOME/aquafin` (default `~/.config/aquafin`).
pub fn config_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().to_path_buf())
}

/// `$XDG_CACHE_HOME/aquafin` (default `~/.cache/aquafin`).
pub fn cache_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.cache_dir().to_path_buf())
}

/// `$XDG_DATA_HOME/aquafin` (default `~/.local/share/aquafin`).
pub fn data_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().to_path_buf())
}

/// `$XDG_STATE_HOME/aquafin` (default `~/.local/state/aquafin`).
pub fn state_dir() -> Result<PathBuf> {
    let dirs = project_dirs()?;
    Ok(dirs
        .state_dir()
        .unwrap_or_else(|| dirs.data_dir())
        .to_path_buf())
}

/// Theme directory: `<config_dir>/themes`.
pub fn themes_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("themes"))
}

/// User executable directory: `$XDG_BIN_HOME` when set to an absolute path,
/// otherwise `~/.local/bin`. (Not part of the XDG base spec, so resolved here.)
pub fn bin_dir() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_BIN_HOME") {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let base = BaseDirs::new().context("could not determine home directory (is $HOME set?)")?;
    Ok(base.home_dir().join(".local").join("bin"))
}

/// Config file: `<config_dir>/config.toml`.
pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Credentials file: `<data_dir>/credentials.toml`.
pub fn credentials_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("credentials.toml"))
}

/// Log file: `<state_dir>/aquafin.log`.
pub fn log_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("aquafin.log"))
}
