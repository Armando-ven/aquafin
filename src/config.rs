//! Configuration schema and persistence.
//!
//! Load order is built-in defaults → `config.toml` → CLI flags (the CLI override
//! happens at the call site, e.g. `--log-level` in `main`). Missing sections and
//! fields fall back to defaults silently via `#[serde(default)]`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::LogLevel;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub ui: UiConfig,
    /// Action-name → key-string overrides (e.g. `down = "j, down"`).
    pub keymap: BTreeMap<String, String>,
    pub audio: AudioConfig,
    pub log: LogConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub url: String,
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub image_protocol: ImageProtocol,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "default".to_string(),
            image_protocol: ImageProtocol::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageProtocol {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Ascii,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub volume: u8,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { volume: 100 }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// Log verbosity; the `--log-level` CLI flag overrides this.
    pub level: Option<LogLevel>,
    /// How many rotated log files to keep.
    pub max_files: Option<usize>,
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        crate::paths::config_file()
    }

    /// Whether a config file exists (i.e. the app has been set up before).
    pub fn exists() -> bool {
        Self::path().map(|path| path.exists()).unwrap_or(false)
    }

    pub fn save(&self) -> Result<()> {
        save_to(&Self::path()?, self)
    }

    pub fn load() -> Result<Option<Config>> {
        load_from(&Self::path()?)
    }
}

fn save_to(path: &Path, config: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized = toml::to_string_pretty(config).context("serializing config")?;
    std::fs::write(path, serialized).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn load_from(path: &Path) -> Result<Option<Config>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(toml::from_str(&contents).context("parsing config")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("reading config"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config {
            server: ServerConfig {
                url: "https://jelly.example".to_string(),
                user_id: "u1".to_string(),
            },
            ..Default::default()
        };
        config.ui.theme = "catppuccin-mocha".to_string();
        config.keymap.insert("down".to_string(), "j, down".to_string());

        save_to(&path, &config).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.server.url, "https://jelly.example");
        assert_eq!(loaded.ui.theme, "catppuccin-mocha");
        assert_eq!(loaded.keymap.get("down").map(String::as_str), Some("j, down"));
    }

    #[test]
    fn missing_file_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_from(&dir.path().join("absent.toml")).unwrap().is_none());
    }

    #[test]
    fn empty_config_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.server.url, "");
        assert_eq!(loaded.ui.theme, "default");
        assert_eq!(loaded.ui.image_protocol, ImageProtocol::Auto);
        assert_eq!(loaded.audio.volume, 100);
        assert!(loaded.keymap.is_empty());
    }

    #[test]
    fn deleting_a_section_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Only [server]; [ui], [audio], [log], [keymap] omitted entirely.
        std::fs::write(&path, "[server]\nurl = \"http://x\"\n").unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.server.url, "http://x");
        assert_eq!(loaded.ui.theme, "default");
        assert_eq!(loaded.audio.volume, 100);
    }
}
