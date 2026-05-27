//! Jellyfin HTTP API client: authentication, read-only item queries, and
//! playback reporting. All network I/O is async; errors surface as [`Error`].

pub mod auth;
pub mod client;
pub mod items;
pub mod models;
pub mod playback;

pub use client::JellyfinClient;

/// Errors produced by the API layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("server returned status {status}: {message}")]
    Status { status: u16, message: String },

    #[error("authentication failed (invalid credentials)")]
    AuthFailed,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("OS keyring error: {0}")]
    Keyring(String),

    #[error("TOML error: {0}")]
    Toml(String),

    #[error("path resolution error: {0}")]
    Path(String),
}

pub type Result<T> = std::result::Result<T, Error>;
