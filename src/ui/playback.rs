//! The playback controller: the bridge between the (pure, I/O-free) [`App`] and
//! the side-effecting players.
//!
//! It owns the Jellyfin client, the in-app [`AudioEngine`], and the current mpv
//! [`VideoSession`], and turns [`Intent`]s the UI queued into actual playback.
//! All network and IPC work happens on the async runtime or on the audio thread,
//! never on the UI thread; the UI reads back a [`NowPlaying`] snapshot each tick.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::{Handle, Runtime};

use crate::api::models::{PlaybackProgressInfo, PlaybackStartInfo, PlaybackStopInfo};
use crate::api::JellyfinClient;
use crate::audio::{
    AudioEngine, AudioMonitor, TrackMeta, SUPPORTED_AUDIO_CODECS, SUPPORTED_CONTAINERS,
};
use crate::video::{self, VideoError, VideoSession};

use super::app::{App, Intent, Item, MediaKind, NowPlaying};

const TICKS_PER_SECOND: f64 = 10_000_000.0;
const PROGRESS_INTERVAL_SECS: u64 = 10;
const VOLUME_STEP: i16 = 5;

/// Outcome of the async track download, delivered back to the UI thread.
enum FetchResult {
    Ready { bytes: Vec<u8>, meta: TrackMeta },
    Failed(String),
    /// The server rejected a favorite toggle; the optimistic flip in the UI
    /// must be reverted to `previous`.
    FavoriteFailed {
        item_id: String,
        previous: bool,
        error: String,
    },
}

/// A running mpv session plus the bookkeeping the controller keeps alongside it.
struct VideoPlayback {
    session: VideoSession,
    item: Item,
    /// Tells the reporter task to post `Stopped` and exit.
    stop: Arc<AtomicBool>,
    /// Latest position (ms), written by the reporter, read for the now-playing bar.
    position_ms: Arc<AtomicU64>,
}

/// The current in-app audio track and its reporter's stop signal.
struct AudioSession {
    stop: Arc<AtomicBool>,
}

pub struct Playback {
    rt: Handle,
    client: JellyfinClient,
    audio: AudioEngine,
    video: Option<VideoPlayback>,
    audio_session: Option<AudioSession>,
    fetch_tx: Sender<FetchResult>,
    fetch_rx: Receiver<FetchResult>,
    seek_seconds: u32,
}

impl Playback {
    pub fn new(rt: Handle, client: JellyfinClient, audio: AudioEngine, seek_seconds: u32) -> Self {
        let (fetch_tx, fetch_rx) = mpsc::channel();
        Self {
            rt,
            client,
            audio,
            video: None,
            audio_session: None,
            fetch_tx,
            fetch_rx,
            seek_seconds,
        }
    }

    /// Perform one queued side effect.
    pub fn dispatch(&mut self, intent: Intent, app: &mut App) {
        match intent {
            Intent::Play { item, media } => match media {
                MediaKind::Video => self.play_video(item, app),
                MediaKind::Audio => {
                    // Build a queue from the active level so the engine can
                    // auto-advance to the next track without user input.
                    app.build_queue_for(&item);
                    self.start_audio(item, app);
                }
                MediaKind::Other => {}
            },
            Intent::TogglePause => self.toggle_pause(),
            Intent::Stop => {
                app.clear_queue();
                self.stop_audio();
            }
            Intent::VolumeUp => {
                self.audio.nudge_volume(VOLUME_STEP);
                app.queue_intent(Intent::SaveVolume(self.audio.current_volume()));
            }
            Intent::VolumeDown => {
                self.audio.nudge_volume(-VOLUME_STEP);
                app.queue_intent(Intent::SaveVolume(self.audio.current_volume()));
            }
            Intent::SeekForward => self.seek(self.seek_seconds as i32),
            Intent::SeekBackward => self.seek(-(self.seek_seconds as i32)),
            Intent::SetFavorite { item_id, favorite } => self.set_favorite(item_id, favorite, app),
            Intent::QueueNext => {
                if let Some(next) = app.advance_queue() {
                    self.stop_audio();
                    self.start_audio(next, app);
                }
            }
            Intent::QueuePrev => {
                if let Some(prev) = app.previous_in_queue() {
                    self.stop_audio();
                    self.start_audio(prev, app);
                }
            }
            // Folder drilling / section refetch / search are handled by the
            // browser; theme switches and pref saves by the run loop, not
            // playback.
            Intent::OpenFolder { .. }
            | Intent::ApplySection { .. }
            | Intent::Search { .. }
            | Intent::SetTheme(_)
            | Intent::SaveAudioPrefs { .. }
            | Intent::SaveVolume(_)
            | Intent::SaveSectionMemory(_)
            | Intent::SaveLastLibrary(_)
            | Intent::SaveSearchHistory(_) => {}
        }
    }

    /// Pause/resume whichever is active: prefer mpv when a video session is up,
    /// otherwise the in-app audio.
    fn toggle_pause(&mut self) {
        if let Some(video) = &self.video {
            if let Err(e) = video::toggle_pause(video.session.socket_path()) {
                tracing::warn!(error = %e, "mpv pause toggle failed");
            }
        } else {
            self.audio.toggle();
        }
    }

    /// Seek `delta_secs` (positive forward, negative backward) on whatever is
    /// playing. No-op if nothing is.
    fn seek(&mut self, delta_secs: i32) {
        if let Some(video) = &self.video {
            if let Err(e) = video::seek_relative(video.session.socket_path(), delta_secs) {
                tracing::warn!(error = %e, "mpv seek failed");
            }
        } else if self.audio.has_track() {
            self.audio.seek_relative(delta_secs);
        }
    }

    /// POST/DELETE the favorite to Jellyfin; on failure, revert the app's
    /// optimistic local flip and show an error.
    fn set_favorite(&self, item_id: String, favorite: bool, app: &mut App) {
        let client = self.client.clone();
        let tx = self.fetch_tx.clone();
        let id = item_id.clone();
        self.rt.spawn(async move {
            if let Err(e) = client.set_favorite(&id, favorite).await {
                tracing::warn!(error = %e, item_id = %id, favorite, "favorite request failed");
                let _ = tx.send(FetchResult::FavoriteFailed {
                    item_id: id,
                    previous: !favorite,
                    error: e.to_string(),
                });
            }
        });
        // Touch `app` for symmetry with the other dispatch arms (no UI change
        // needed: the toggle was applied optimistically in handle_key).
        let _ = app;
    }

    /// Per-frame housekeeping: collect finished downloads, notice mpv/audio
    /// ending, surface late errors, and refresh the now-playing snapshot.
    pub fn tick(&mut self, app: &mut App) {
        while let Ok(result) = self.fetch_rx.try_recv() {
            match result {
                FetchResult::Ready { bytes, meta } => self.begin_audio(bytes, meta),
                FetchResult::Failed(message) => app.show_error(message),
                FetchResult::FavoriteFailed { item_id, previous, error } => {
                    app.revert_favorite(&item_id, previous);
                    app.show_error(format!("Couldn't update favorite: {error}"));
                }
            }
        }

        // mpv closed by the user?
        if let Some(video) = &mut self.video {
            if video.session.has_exited() {
                video.stop.store(true, Ordering::SeqCst);
                self.video = None;
            }
        }

        // Track finished on its own? Its reporter notices the engine went idle
        // and posts Stopped; we drop our handle and, if a queue is set, kick
        // off the next track.
        if self.audio.take_finished() {
            self.audio_session = None;
            if let Some(next) = app.advance_queue() {
                self.start_audio(next, app);
            }
        }

        if let Some(error) = self.audio.last_error() {
            app.show_error(error);
            self.audio_session = None;
        }

        app.now_playing = self.now_playing();
    }

    /// On the way out, stop the reporters and make a best-effort synchronous
    /// `Stopped` report so the server doesn't think we're still playing.
    pub fn shutdown(&mut self, runtime: &Runtime) {
        if let Some(video) = self.video.take() {
            video.stop.store(true, Ordering::SeqCst);
            let ticks = (video.position_ms.load(Ordering::Relaxed) as f64 / 1000.0
                * TICKS_PER_SECOND) as i64;
            let _ = runtime.block_on(self.client.report_playback_stopped(&PlaybackStopInfo {
                item_id: video.item.id,
                position_ticks: Some(ticks),
                ..Default::default()
            }));
        }
        if let Some(audio) = self.audio_session.take() {
            audio.stop.store(true, Ordering::SeqCst);
            let monitor = self.audio.monitor();
            if let Some(item_id) = monitor.current_item_id() {
                let ticks = (monitor.position().as_secs_f64() * TICKS_PER_SECOND) as i64;
                let _ = runtime.block_on(self.client.report_playback_stopped(&PlaybackStopInfo {
                    item_id,
                    position_ticks: Some(ticks),
                    ..Default::default()
                }));
            }
        }
        self.audio.stop();
    }

    // --- video ---------------------------------------------------------------

    fn play_video(&mut self, item: Item, app: &mut App) {
        self.stop_audio(); // don't stack in-app audio under a video
        let url = self.client.video_stream_url(&item.id);
        match video::spawn(&url, &item.id, &item.name) {
            Ok(session) => {
                if let Some(mut previous) = self.video.take() {
                    previous.stop.store(true, Ordering::SeqCst);
                    previous.session.kill();
                }
                let stop = Arc::new(AtomicBool::new(false));
                let position_ms = Arc::new(AtomicU64::new(0));
                self.spawn_video_reporter(
                    item.id.clone(),
                    session.socket_path().to_path_buf(),
                    Arc::clone(&stop),
                    Arc::clone(&position_ms),
                );
                app.set_status(format!("Playing in mpv: {}", item.name));
                self.video = Some(VideoPlayback {
                    session,
                    item,
                    stop,
                    position_ms,
                });
            }
            Err(VideoError::MpvNotInstalled) => {
                app.show_error("mpv is not installed or not on PATH. Install mpv to play video.");
            }
            Err(e) => app.show_error(format!("Couldn't start mpv: {e}")),
        }
    }

    fn spawn_video_reporter(
        &self,
        item_id: String,
        socket_path: PathBuf,
        stop: Arc<AtomicBool>,
        position_ms: Arc<AtomicU64>,
    ) {
        let client = self.client.clone();
        self.rt.spawn(async move {
            report_start(&client, &item_id, true, "DirectPlay").await;

            let mut last_ticks = 0i64;
            let mut elapsed = 0u64;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                elapsed += 1;

                let path = socket_path.clone();
                match tokio::task::spawn_blocking(move || video::query_time_pos(&path)).await {
                    Ok(Ok(Some(secs))) => {
                        position_ms.store((secs * 1000.0) as u64, Ordering::Relaxed);
                        last_ticks = (secs * TICKS_PER_SECOND) as i64;
                        if elapsed.is_multiple_of(PROGRESS_INTERVAL_SECS) {
                            report_progress(&client, &item_id, last_ticks, false, "DirectPlay", None)
                                .await;
                        }
                    }
                    Ok(Ok(None)) => {} // connected but not playing yet
                    Ok(Err(_)) | Err(_) => break, // socket gone ⇒ mpv exited
                }
            }
            report_stopped(&client, &item_id, last_ticks).await;
        });
    }

    // --- audio ---------------------------------------------------------------

    fn start_audio(&mut self, item: Item, app: &mut App) {
        if !self.audio.available() {
            app.show_error("No audio output device is available.");
            return;
        }
        let client = self.client.clone();
        let tx = self.fetch_tx.clone();
        let id = item.id.clone();
        let meta = TrackMeta {
            item_id: item.id.clone(),
            title: item.name.clone(),
            subtitle: None,
        };
        app.set_status(format!("Loading: {}", item.name));
        self.rt.spawn(async move {
            let result = match client
                .audio_bytes(&id, SUPPORTED_CONTAINERS, SUPPORTED_AUDIO_CODECS)
                .await
            {
                Ok(bytes) => FetchResult::Ready { bytes, meta },
                Err(e) => {
                    tracing::warn!(error = %e, item_id = %id, "audio download failed");
                    FetchResult::Failed(format!("Couldn't load track: {e}"))
                }
            };
            let _ = tx.send(result);
        });
    }

    /// A download finished: hand it to the engine and (re)start the reporter.
    fn begin_audio(&mut self, bytes: Vec<u8>, meta: TrackMeta) {
        if let Some(previous) = self.audio_session.take() {
            previous.stop.store(true, Ordering::SeqCst);
        }
        let item_id = meta.item_id.clone();
        self.audio.play(bytes, meta);
        let stop = Arc::new(AtomicBool::new(false));
        self.spawn_audio_reporter(item_id, Arc::clone(&stop), self.audio.monitor());
        self.audio_session = Some(AudioSession { stop });
    }

    fn stop_audio(&mut self) {
        self.audio.stop();
        if let Some(previous) = self.audio_session.take() {
            previous.stop.store(true, Ordering::SeqCst);
        }
    }

    fn spawn_audio_reporter(&self, item_id: String, stop: Arc<AtomicBool>, monitor: AudioMonitor) {
        let client = self.client.clone();
        self.rt.spawn(async move {
            report_start(&client, &item_id, false, "DirectStream").await;

            let mut last_ticks = 0i64;
            let mut elapsed = 0u64;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if stop.load(Ordering::SeqCst) || !monitor.is_active() {
                    break;
                }
                elapsed += 1;
                last_ticks = (monitor.position().as_secs_f64() * TICKS_PER_SECOND) as i64;
                if elapsed.is_multiple_of(PROGRESS_INTERVAL_SECS) {
                    report_progress(
                        &client,
                        &item_id,
                        last_ticks,
                        false,
                        "DirectStream",
                        Some(monitor.volume() as i32),
                    )
                    .await;
                }
            }
            report_stopped(&client, &item_id, last_ticks).await;
        });
    }

    // --- now-playing snapshot ------------------------------------------------

    fn now_playing(&self) -> Option<NowPlaying> {
        if let Some(video) = &self.video {
            return Some(NowPlaying {
                item_id: video.item.id.clone(),
                kind: MediaKind::Video,
                title: video.item.name.clone(),
                subtitle: Some("Direct play in mpv".to_string()),
                position: Duration::from_millis(video.position_ms.load(Ordering::Relaxed)),
                duration: video.item.run_time_ticks.and_then(ticks_to_duration),
                paused: false,
                volume: None,
            });
        }
        if self.audio_session.is_some() {
            let snapshot = self.audio.snapshot();
            if let Some(track) = snapshot.track {
                return Some(NowPlaying {
                    item_id: track.item_id,
                    kind: MediaKind::Audio,
                    title: track.title,
                    subtitle: track.subtitle,
                    position: snapshot.position,
                    duration: snapshot.duration,
                    paused: snapshot.paused,
                    volume: Some(snapshot.volume),
                });
            }
        }
        None
    }
}

/// Jellyfin `RunTimeTicks` (100 ns units) → a `Duration`, if positive.
fn ticks_to_duration(ticks: i64) -> Option<Duration> {
    (ticks > 0).then(|| Duration::from_secs_f64(ticks as f64 / TICKS_PER_SECOND))
}

// Reporting helpers: all best-effort — a failed report is logged, never fatal.

async fn report_start(client: &JellyfinClient, item_id: &str, can_seek: bool, method: &str) {
    let info = PlaybackStartInfo {
        item_id: item_id.to_string(),
        position_ticks: Some(0),
        is_paused: false,
        can_seek,
        play_method: Some(method.to_string()),
        ..Default::default()
    };
    if let Err(e) = client.report_playback_start(&info).await {
        tracing::warn!(error = %e, item_id, "playback start report failed");
    }
}

async fn report_progress(
    client: &JellyfinClient,
    item_id: &str,
    position_ticks: i64,
    is_paused: bool,
    method: &str,
    volume_level: Option<i32>,
) {
    let info = PlaybackProgressInfo {
        item_id: item_id.to_string(),
        position_ticks: Some(position_ticks),
        is_paused,
        play_method: Some(method.to_string()),
        volume_level,
        ..Default::default()
    };
    if let Err(e) = client.report_playback_progress(&info).await {
        tracing::warn!(error = %e, item_id, "playback progress report failed");
    }
}

async fn report_stopped(client: &JellyfinClient, item_id: &str, position_ticks: i64) {
    let info = PlaybackStopInfo {
        item_id: item_id.to_string(),
        position_ticks: Some(position_ticks),
        ..Default::default()
    };
    if let Err(e) = client.report_playback_stopped(&info).await {
        tracing::warn!(error = %e, item_id, "playback stopped report failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_convert_to_duration() {
        assert_eq!(ticks_to_duration(0), None);
        assert_eq!(ticks_to_duration(-5), None);
        // 1 second == 10,000,000 ticks.
        assert_eq!(ticks_to_duration(10_000_000), Some(Duration::from_secs(1)));
    }

    #[test]
    fn volume_intents_reach_the_engine() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = JellyfinClient::new("http://localhost", "tok", "u1", "dev").unwrap();
        let audio = AudioEngine::new(50);
        let mut playback = Playback::new(runtime.handle().clone(), client, audio, 5);

        let mut app = App::new();
        playback.dispatch(Intent::VolumeUp, &mut app);
        playback.dispatch(Intent::VolumeUp, &mut app);
        // The shared volume is updated synchronously by each nudge, so two +5
        // steps from 50 land on 60 regardless of whether a device exists.
        assert_eq!(playback.audio.snapshot().volume, 60);
    }
}
