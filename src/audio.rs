//! In-app audio playback.
//!
//! Music plays inside aquafin (no external player) over the system audio stack
//! via `rodio`, which decodes with `symphonia` under the hood. rodio's output
//! objects are not `Send`, so they live on a dedicated thread; the UI talks to
//! that thread through a command channel and reads playback state from shared
//! atomics. Tracks are fetched as bytes and decoded from memory.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rodio::{Decoder, Source};

/// Containers aquafin can decode locally, advertised to the Jellyfin universal
/// audio endpoint. `ogg` is deliberately excluded: an ogg container can hold
/// Opus (which we can't decode), and if `ogg` is whitelisted the server
/// direct-streams it regardless of the codec whitelist below. Leaving it out
/// makes ogg sources transcode to `aac` instead.
pub const SUPPORTED_CONTAINERS: &str = "mp3,aac,m4a,flac,wav";

/// Audio codecs aquafin can decode (via rodio/symphonia). Opus is deliberately
/// absent: symphonia can't decode it, so the server transcodes Opus sources to
/// the *first* codec here (aac — compact and reliably decoded). Listed codecs
/// direct-stream, so FLAC and friends keep their original quality.
pub const SUPPORTED_AUDIO_CODECS: &str = "aac,mp3,flac,alac,vorbis,pcm";

/// Identifying metadata for the track shown in the now-playing UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMeta {
    pub item_id: String,
    pub title: String,
    pub subtitle: Option<String>,
}

/// Commands sent to the audio thread.
enum Command {
    Play { bytes: Vec<u8>, meta: TrackMeta },
    Toggle,
    Stop,
    SetVolume(u8),
    /// Seek by `delta_secs` relative to the current position; clamped to ≥ 0.
    SeekRelative(i32),
    Shutdown,
}

/// Playback state shared between the audio thread, the UI, and the reporter.
#[derive(Debug)]
struct Shared {
    /// Whether an output device was acquired. False ⇒ this machine has no audio.
    available: AtomicBool,
    /// A track is loaded (playing or paused).
    has_track: AtomicBool,
    paused: AtomicBool,
    /// The current track ran to its natural end (consumed by the reporter).
    finished: AtomicBool,
    position_ms: AtomicU64,
    /// 0 means "unknown duration".
    duration_ms: AtomicU64,
    volume: AtomicU8,
    track: Mutex<Option<TrackMeta>>,
    last_error: Mutex<Option<String>>,
}

impl Shared {
    fn new(volume: u8) -> Self {
        Self {
            available: AtomicBool::new(false),
            has_track: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            position_ms: AtomicU64::new(0),
            duration_ms: AtomicU64::new(0),
            volume: AtomicU8::new(volume.min(100)),
            track: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }
}

/// A point-in-time view of audio playback for the UI.
#[derive(Debug, Clone)]
pub struct AudioSnapshot {
    pub track: Option<TrackMeta>,
    pub paused: bool,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: u8,
}

/// Handle to the audio playback thread. Dropping it stops playback and joins.
pub struct AudioEngine {
    tx: Sender<Command>,
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl AudioEngine {
    /// Spawn the audio thread and wait for it to acquire (or fail to acquire) an
    /// output device, so [`AudioEngine::available`] is accurate on return.
    pub fn new(initial_volume: u8) -> Self {
        let shared = Arc::new(Shared::new(initial_volume));
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<()>();

        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("aquafin-audio".into())
            .spawn(move || audio_thread(rx, thread_shared, ready_tx))
            .expect("spawn audio thread");

        // Block briefly for the device-init handshake.
        let _ = ready_rx.recv_timeout(Duration::from_secs(2));

        Self {
            tx,
            shared,
            handle: Some(handle),
        }
    }

    pub fn available(&self) -> bool {
        self.shared.available.load(Ordering::Relaxed)
    }

    /// Start playing `bytes` (a full encoded track held in memory).
    pub fn play(&self, bytes: Vec<u8>, meta: TrackMeta) {
        let _ = self.tx.send(Command::Play { bytes, meta });
    }

    /// Pause if playing, resume if paused.
    pub fn toggle(&self) {
        let _ = self.tx.send(Command::Toggle);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }

    /// Seek by `delta_secs` from the current position. Negative seeks back; the
    /// resulting position is clamped at the start of the track.
    pub fn seek_relative(&self, delta_secs: i32) {
        let _ = self.tx.send(Command::SeekRelative(delta_secs));
    }

    /// True if a track is loaded (playing or paused) — UI uses this to decide
    /// whether seek/pause feedback applies.
    pub fn has_track(&self) -> bool {
        self.shared.has_track.load(Ordering::Relaxed)
    }

    /// Current volume (0..=100). Useful for persisting to config after a
    /// `nudge_volume` call.
    pub fn current_volume(&self) -> u8 {
        self.shared.volume.load(Ordering::Relaxed)
    }

    /// Adjust volume by `delta` percentage points, clamped to 0..=100.
    pub fn nudge_volume(&self, delta: i16) {
        // Update the shared value immediately (atomically) so rapid nudges
        // compound correctly even before the thread applies each one, then send
        // the resulting absolute level.
        let mut next = self.shared.volume.load(Ordering::Relaxed);
        let _ = self
            .shared
            .volume
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                next = (current as i16 + delta).clamp(0, 100) as u8;
                Some(next)
            });
        let _ = self.tx.send(Command::SetVolume(next));
    }

    /// Take the "track finished on its own" flag (true at most once per track).
    pub fn take_finished(&self) -> bool {
        self.shared.finished.swap(false, Ordering::AcqRel)
    }

    pub fn last_error(&self) -> Option<String> {
        self.shared.last_error.lock().unwrap().take()
    }

    /// A cheap, cloneable, `Send` view of playback state for the progress
    /// reporter to poll without holding the (non-`Send`) engine.
    pub fn monitor(&self) -> AudioMonitor {
        AudioMonitor(Arc::clone(&self.shared))
    }

    pub fn snapshot(&self) -> AudioSnapshot {
        let dur_ms = self.shared.duration_ms.load(Ordering::Relaxed);
        AudioSnapshot {
            track: self.shared.track.lock().unwrap().clone(),
            paused: self.shared.paused.load(Ordering::Relaxed),
            position: Duration::from_millis(self.shared.position_ms.load(Ordering::Relaxed)),
            duration: (dur_ms > 0).then(|| Duration::from_millis(dur_ms)),
            volume: self.shared.volume.load(Ordering::Relaxed),
        }
    }
}

/// A cloneable, thread-safe handle to read playback state. Used by the server
/// progress reporter, which lives on the async runtime rather than the UI thread.
#[derive(Clone)]
pub struct AudioMonitor(Arc<Shared>);

impl AudioMonitor {
    /// The item id of the track currently loaded, if any.
    pub fn current_item_id(&self) -> Option<String> {
        self.0.track.lock().unwrap().as_ref().map(|m| m.item_id.clone())
    }

    /// Whether a track is loaded (playing or paused).
    pub fn is_active(&self) -> bool {
        self.0.has_track.load(Ordering::Relaxed)
    }

    pub fn position(&self) -> Duration {
        Duration::from_millis(self.0.position_ms.load(Ordering::Relaxed))
    }

    pub fn volume(&self) -> u8 {
        self.0.volume.load(Ordering::Relaxed)
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Convert a 0..=100 volume to rodio's linear gain (1.0 == unaltered).
fn volume_to_gain(volume: u8) -> f32 {
    volume.min(100) as f32 / 100.0
}

/// The body of the audio thread: owns the rodio device + player and services
/// commands, updating shared state between commands so the UI stays current.
fn audio_thread(rx: mpsc::Receiver<Command>, shared: Arc<Shared>, ready: Sender<()>) {
    let mut device = match rodio::DeviceSinkBuilder::open_default_sink() {
        Ok(device) => device,
        Err(e) => {
            *shared.last_error.lock().unwrap() = Some(format!("no audio output device: {e}"));
            let _ = ready.send(());
            // Drain commands so senders never block; we just can't play anything.
            while let Ok(cmd) = rx.recv() {
                if matches!(cmd, Command::Shutdown) {
                    break;
                }
            }
            return;
        }
    };

    // rodio otherwise prints a notice to stderr when the sink drops, which would
    // corrupt the alternate-screen TUI on shutdown.
    device.log_on_drop(false);
    let player = rodio::Player::connect_new(device.mixer());
    player.set_volume(volume_to_gain(shared.volume.load(Ordering::Relaxed)));
    shared.available.store(true, Ordering::Relaxed);
    let _ = ready.send(());

    // We hang on to the current track's encoded bytes so seeks (especially
    // backward) can rebuild the decoder from zero — rodio's try_seek on streamed
    // decoders is unreliable backward across codecs, so we re-decode + skip
    // forward to the target, which works for every format we accept.
    let mut current_bytes: Option<Arc<Vec<u8>>> = None;

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Command::Play { bytes, meta }) => {
                player.clear();
                let bytes = Arc::new(bytes);
                match Decoder::new(Cursor::new(bytes.as_ref().clone())) {
                    Ok(decoder) => {
                        let duration = decoder.total_duration();
                        shared
                            .duration_ms
                            .store(duration.map_or(0, |d| d.as_millis() as u64), Ordering::Relaxed);
                        shared.position_ms.store(0, Ordering::Relaxed);
                        *shared.track.lock().unwrap() = Some(meta);
                        shared.has_track.store(true, Ordering::Relaxed);
                        shared.paused.store(false, Ordering::Relaxed);
                        shared.finished.store(false, Ordering::Relaxed);
                        current_bytes = Some(bytes);
                        player.append(decoder);
                        player.play();
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "audio decode failed");
                        *shared.last_error.lock().unwrap() =
                            Some(format!("could not decode this track: {e}"));
                        shared.has_track.store(false, Ordering::Relaxed);
                        *shared.track.lock().unwrap() = None;
                        current_bytes = None;
                    }
                }
            }
            Ok(Command::Toggle) => {
                if shared.has_track.load(Ordering::Relaxed) {
                    if player.is_paused() {
                        player.play();
                        shared.paused.store(false, Ordering::Relaxed);
                    } else {
                        player.pause();
                        shared.paused.store(true, Ordering::Relaxed);
                    }
                }
            }
            Ok(Command::Stop) => {
                player.clear();
                clear_track(&shared);
                current_bytes = None;
            }
            Ok(Command::SetVolume(v)) => {
                player.set_volume(volume_to_gain(v));
                shared.volume.store(v, Ordering::Relaxed);
            }
            Ok(Command::SeekRelative(delta_secs)) => {
                if shared.has_track.load(Ordering::Relaxed) {
                    let current_ms = shared.position_ms.load(Ordering::Relaxed) as i64;
                    let target_ms = (current_ms + (delta_secs as i64) * 1000).max(0) as u64;
                    let target = Duration::from_millis(target_ms);
                    let was_paused = shared.paused.load(Ordering::Relaxed);
                    let rebuilt = if delta_secs < 0 {
                        // Backward seeks: rebuild from the start. try_seek on a
                        // streamed Decoder is unreliable across codecs going
                        // backward; re-decoding + forward-seeking always works.
                        rebuild_and_seek(&player, current_bytes.as_ref(), target)
                    } else {
                        // Forward: try in-place seek first; if that fails, fall
                        // back to rebuild (same reasoning).
                        match player.try_seek(target) {
                            Ok(()) => true,
                            Err(e) => {
                                tracing::debug!(error = %e, "in-place seek failed; rebuilding");
                                rebuild_and_seek(&player, current_bytes.as_ref(), target)
                            }
                        }
                    };
                    if rebuilt {
                        shared.position_ms.store(target_ms, Ordering::Relaxed);
                        if was_paused {
                            player.pause();
                        }
                    } else {
                        tracing::warn!("audio seek failed");
                    }
                }
            }
            Ok(Command::Shutdown) => {
                player.stop();
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Between commands: refresh position and detect natural end of track.
        if shared.has_track.load(Ordering::Relaxed) {
            shared
                .position_ms
                .store(player.get_pos().as_millis() as u64, Ordering::Relaxed);
            if player.empty() {
                shared.finished.store(true, Ordering::Relaxed);
                clear_track(&shared);
            }
        }
    }
}

/// Rebuild the decoder from the cached bytes and seek to `target` by skipping
/// from the start. Returns `false` if there are no cached bytes, decoding fails,
/// or the forward seek itself fails — leaving the player cleared.
fn rebuild_and_seek(
    player: &rodio::Player,
    bytes: Option<&Arc<Vec<u8>>>,
    target: Duration,
) -> bool {
    let Some(bytes) = bytes else {
        return false;
    };
    let decoder = match Decoder::new(Cursor::new(bytes.as_ref().clone())) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "audio re-decode for seek failed");
            return false;
        }
    };
    player.clear();
    player.append(decoder);
    // After clear(), the player is paused; play and then seek forward to target.
    player.play();
    if target.is_zero() {
        return true;
    }
    if let Err(e) = player.try_seek(target) {
        tracing::warn!(error = %e, "forward seek on rebuilt decoder failed");
        return false;
    }
    true
}

fn clear_track(shared: &Shared) {
    shared.has_track.store(false, Ordering::Relaxed);
    shared.paused.store(false, Ordering::Relaxed);
    shared.position_ms.store(0, Ordering::Relaxed);
    shared.duration_ms.store(0, Ordering::Relaxed);
    *shared.track.lock().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_maps_to_unit_gain() {
        assert_eq!(volume_to_gain(0), 0.0);
        assert_eq!(volume_to_gain(100), 1.0);
        assert_eq!(volume_to_gain(50), 0.5);
        // Clamps above 100.
        assert_eq!(volume_to_gain(200), 1.0);
    }

    #[test]
    fn nudge_volume_clamps_and_reports_in_snapshot() {
        // Constructing the engine is safe even without an audio device (the
        // thread reports unavailable and drains commands).
        let engine = AudioEngine::new(90);
        assert_eq!(engine.snapshot().volume, 90);
        engine.nudge_volume(20); // would be 110, clamps to 100
        // Give the thread a moment to apply it.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(engine.snapshot().volume, 100);
        engine.nudge_volume(-130); // clamps to 0
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(engine.snapshot().volume, 0);
    }

    #[test]
    fn snapshot_is_empty_before_playing() {
        let engine = AudioEngine::new(100);
        let snap = engine.snapshot();
        assert!(snap.track.is_none());
        assert!(!snap.paused);
        assert_eq!(snap.position, Duration::ZERO);
        assert!(snap.duration.is_none());
    }
}
