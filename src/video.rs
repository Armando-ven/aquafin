//! External video playback via a spawned `mpv` process.
//!
//! aquafin does not decode video itself: it launches `mpv` in its own window,
//! pointed at a Jellyfin direct-play URL, and talks to it over the JSON IPC
//! socket (`--input-ipc-server`) to read the playback position for progress
//! reporting. The TUI keeps running while mpv is alive; closing mpv ends the
//! session.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Why a video failed to start.
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("mpv is not installed or not on PATH. Install mpv to play video.")]
    MpvNotInstalled,

    #[error("failed to launch mpv: {0}")]
    Spawn(#[source] std::io::Error),
}

/// A live mpv process plus the path to its IPC socket.
#[derive(Debug)]
pub struct VideoSession {
    item_id: String,
    title: String,
    socket_path: PathBuf,
    child: Child,
}

impl VideoSession {
    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Has mpv exited? Reaps the child without blocking when it has.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }

    /// Terminate mpv (used when replacing it with another video).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Ask mpv (over IPC) for the current playback position, in seconds.
    pub fn position_secs(&self) -> Option<f64> {
        query_time_pos(&self.socket_path).ok().flatten()
    }
}

/// Read mpv's `time-pos` over its IPC socket. A free function so the progress
/// reporter can poll a cloned socket path off the UI thread without holding the
/// [`VideoSession`]. Distinguishes the cases the reporter needs:
/// - `Err(_)` — the socket is gone (mpv exited),
/// - `Ok(None)` — connected, but no position yet (still loading),
/// - `Ok(Some(secs))` — the current position.
pub fn query_time_pos(socket_path: &Path) -> std::io::Result<Option<f64>> {
    query_f64_property(socket_path, "time-pos")
}

impl Drop for VideoSession {
    /// If aquafin exits while mpv is still up, leave mpv running (it's the user's
    /// window) but clean up the socket file we created.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Launch mpv on `stream_url`, returning the session once the process is spawned.
/// mpv opens its own window; we never block the UI thread on it.
pub fn spawn(stream_url: &str, item_id: &str, title: &str) -> Result<VideoSession, VideoError> {
    let socket_path = socket_path_for(item_id);

    let child = Command::new("mpv")
        .arg(format!("--input-ipc-server={}", socket_path.display()))
        .arg("--force-window=yes")
        .arg("--osc=yes")
        .arg("--no-terminal")
        .arg(format!("--force-media-title={title}"))
        .arg(stream_url)
        // Detach from our stdio so mpv can't scribble over the TUI.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => VideoError::MpvNotInstalled,
            _ => VideoError::Spawn(e),
        })?;

    Ok(VideoSession {
        item_id: item_id.to_string(),
        title: title.to_string(),
        socket_path,
        child,
    })
}

/// A per-session IPC socket under the system temp dir. The item id keeps it
/// readable; our pid keeps concurrent aquafin instances from colliding.
fn socket_path_for(item_id: &str) -> PathBuf {
    let safe: String = item_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    std::env::temp_dir().join(format!("aquafin-mpv-{}-{}.sock", std::process::id(), safe))
}

/// Connect to mpv's IPC socket and read a single numeric property. Returns
/// `Ok(None)` when the property has no value yet (e.g. before playback starts).
fn query_f64_property(socket_path: &Path, property: &str) -> std::io::Result<Option<f64>> {
    const REQUEST_ID: u64 = 1;
    let stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;

    let mut writer = stream.try_clone()?;
    writer.write_all(build_get_property(property, REQUEST_ID).as_bytes())?;
    writer.flush()?;

    // mpv interleaves async event lines with command replies; scan a bounded
    // number of lines for the reply carrying our request id.
    let reader = BufReader::new(stream);
    for line in reader.lines().take(50) {
        let line = line?;
        if let Some(value) = parse_property_response(&line, REQUEST_ID) {
            return Ok(value);
        }
    }
    Ok(None)
}

/// Serialize a `get_property` IPC command line (newline-terminated, as mpv wants).
fn build_get_property(property: &str, request_id: u64) -> String {
    let cmd = serde_json::json!({
        "command": ["get_property", property],
        "request_id": request_id,
    });
    format!("{cmd}\n")
}

/// Parse one IPC reply line. Returns:
/// - `Some(Some(v))` — the matching reply with a numeric value,
/// - `Some(None)` — the matching reply but with no value (property unavailable),
/// - `None` — not the reply we're waiting for (an event or another request).
fn parse_property_response(line: &str, request_id: u64) -> Option<Option<f64>> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("request_id")?.as_u64()? != request_id {
        return None;
    }
    Some(value.get("data").and_then(serde_json::Value::as_f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_property_command_is_newline_terminated_json() {
        let line = build_get_property("time-pos", 1);
        assert!(line.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["command"][0], "get_property");
        assert_eq!(parsed["command"][1], "time-pos");
        assert_eq!(parsed["request_id"], 1);
    }

    #[test]
    fn parses_matching_reply_with_value() {
        let line = r#"{"request_id":1,"error":"success","data":42.5}"#;
        assert_eq!(parse_property_response(line, 1), Some(Some(42.5)));
    }

    #[test]
    fn parses_matching_reply_without_value() {
        // Property not available yet: success but data is null.
        let line = r#"{"request_id":1,"error":"property unavailable","data":null}"#;
        assert_eq!(parse_property_response(line, 1), Some(None));
    }

    #[test]
    fn ignores_events_and_other_request_ids() {
        assert_eq!(
            parse_property_response(r#"{"event":"playback-restart"}"#, 1),
            None
        );
        assert_eq!(
            parse_property_response(r#"{"request_id":2,"data":1.0}"#, 1),
            None
        );
        assert_eq!(parse_property_response("not json", 1), None);
    }

    #[test]
    fn socket_path_sanitizes_item_id() {
        let path = socket_path_for("ab/cd ef");
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("aquafin-mpv-"));
        assert!(name.ends_with("-ab-cd-ef.sock"));
        assert!(!name.contains('/'));
    }
}
