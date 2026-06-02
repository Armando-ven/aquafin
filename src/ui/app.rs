//! Application state and the main TUI event loop.

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};

use super::keymap::{Action, Keymap};
use super::{cheatsheet, error_modal, layout, panes, popup_menu, theme_picker};
use crate::theme::Theme;
use crate::video::{TrackChoice, VideoOptions};

pub(crate) type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Which pane the user is interacting with. The top bar owns library selection
/// (1-9) + search; the four content panes (library items, sections, content,
/// context) take focus for navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    TopBar,
    /// Home view (welcome strip + carousels + library tiles). Takes over the
    /// full main area; when focused, navigation hands off to the home
    /// pane's row/column cursor.
    Home,
    /// Global search view: own input + flat result list across every library.
    /// Opened via `g s`. Like Home, takes over the full main area.
    GlobalSearch,
    LibraryItems,
    LibrarySections,
    Content,
    ContextTop,
    ContextBottom,
}

/// A library item (movie, episode, album, …) with the fields the detail pane shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub overview: Option<String>,
    pub production_year: Option<i32>,
    pub run_time_ticks: Option<i64>,
    pub kind: Option<String>,
    pub primary_image_tag: Option<String>,
    /// A container (series, season, album, artist, …) the user can drill into,
    /// as opposed to a playable leaf.
    pub is_folder: bool,
    pub is_favorite: bool,
    pub is_played: bool,
    /// ReplayGain-style per-track normalization in dB, when the server carries it.
    pub normalization_gain_db: Option<f32>,
    /// Parent album id (set on `Audio` tracks). Drives the "Go to album" menu jump.
    pub album_id: Option<String>,
    /// Album display name (paired with `album_id`).
    pub album_name: Option<String>,
    /// Primary artist reference for the "Go to artist" menu jump. Prefers the
    /// album-artist on tracks/albums and falls back to the first track artist.
    pub primary_artist_id: Option<String>,
    pub primary_artist_name: Option<String>,
}

/// How an item plays back: video opens in mpv, audio plays in-app, everything
/// else (folders, series, …) isn't directly playable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
    Other,
}

impl MediaKind {
    /// Classify from a Jellyfin item `Type`.
    pub fn classify(kind: Option<&str>) -> MediaKind {
        match kind {
            Some("Movie" | "Episode" | "Video" | "MusicVideo" | "Trailer") => MediaKind::Video,
            Some("Audio" | "AudioBook") => MediaKind::Audio,
            _ => MediaKind::Other,
        }
    }
}

/// Kind of a Home dashboard row. Drives which icon + accent + activation
/// semantics the renderer uses for each row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeRowKind {
    Resume,
    NextUp,
    Libraries,
    Recent,
}

/// One horizontal row on the Home dashboard. `library_id` is set on
/// `Recent`/`Libraries` rows for tile activation; `Resume`/`NextUp` rows
/// span every library.
#[derive(Debug, Clone)]
pub struct HomeRow {
    pub kind: HomeRowKind,
    pub title: String,
    pub library_id: Option<String>,
    pub items: Vec<Item>,
}

/// Server-fetched dashboard payload that drives the Home view.
#[derive(Debug, Clone, Default)]
pub struct HomeData {
    pub resume: Vec<Item>,
    pub next_up: Vec<Item>,
    /// `(library_id, name, collection_type, count_label, recent_items)`. One
    /// tuple per visible library, in the same order as `App::libraries`.
    pub recent_per_library: Vec<HomeLibrarySummary>,
    /// Latest items across every library (server-side `/Users/Items` recursive
    /// query sorted by `DateCreated`). Drives the "Latest" row.
    pub latest_global: Vec<Item>,
    /// True until the first fetch lands (lets the renderer show a "Loading…"
    /// placeholder instead of "Empty").
    pub loading: bool,
}

#[derive(Debug, Clone)]
pub struct HomeLibrarySummary {
    pub id: String,
    pub name: String,
    pub collection_type: Option<String>,
    pub item_count: i64,
    pub recent: Vec<Item>,
}

/// Cursor on the Home dashboard. `row` indexes into the dashboard's row list
/// (skipping any row that's empty); `col` indexes into that row's items.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HomeCursor {
    pub row: usize,
    pub col: usize,
}

/// Global search view state: in-progress query + the most recent results.
/// `input_focused` drives whether typing edits the query or moves the result
/// cursor (Up from the first row returns focus to the input).
#[derive(Debug, Clone, Default)]
pub struct GlobalSearchState {
    pub query: String,
    pub results: Vec<Item>,
    pub loading: bool,
    pub selected: usize,
    pub input_focused: bool,
    /// Set once the user has submitted at least one query so the renderer can
    /// distinguish "no results yet" from "no matches".
    pub submitted: bool,
}

/// A side effect requested by the user that the event loop performs (handling
/// the key itself stays pure and I/O-free). Drained each tick via
/// [`App::take_intents`].
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Play this item (already classified as Video or Audio). `start_ticks`
    /// optionally overrides the start position (used for "play from chapter N").
    Play { item: Item, media: MediaKind, start_ticks: Option<i64> },
    /// Load and drill into a folder's children (the loading level is already
    /// pushed; the loader fills it by `id`).
    OpenFolder { id: String, title: String },
    /// Apply a theme by name (loaded by the event loop).
    SetTheme(String),
    TogglePause,
    Stop,
    VolumeUp,
    VolumeDown,
    SeekForward,
    SeekBackward,
    /// Tell the server about a favorite-state change. The app already flipped
    /// the local item optimistically; this carries the desired remote state.
    SetFavorite { item_id: String, favorite: bool },
    /// Tell the server about a played-state change (mirrors `SetFavorite`).
    SetPlayed { item_id: String, played: bool },
    /// Apply a sleep-timer choice (arms an absolute deadline or end-of-track stop).
    SetSleepTimer(SleepTimer),
    /// Build a queue of audio items matching `genre` inside `library_id` and
    /// start playing. Handled by the browser.
    GenreRadio { library_id: String, genre: String },
    /// Create a server-side playlist named `name` carrying `item_ids`.
    CreatePlaylist { name: String, item_ids: Vec<String> },
    /// Toggle gapless audio (persisted + applied at the playback controller).
    SetGapless(bool),
    /// Toggle ReplayGain normalization (persisted + applied to the audio engine).
    SetNormalization(bool),
    /// Pick an EQ preset (persisted + applied to the audio engine).
    SetEqPreset(crate::config::EqPreset),
    /// Refetch the library root level with this section's filter (Enter on a
    /// section). The library's drill stack is reset to a loading root level.
    ApplySection {
        library_id: String,
        section: Section,
        extras: SectionFilters,
    },
    /// Run a search query (Enter inside the search input). Results replace the
    /// active library's root level via `apply_search_results`. `item_types`
    /// scopes the search to the active section's Jellyfin item types (empty =
    /// search every type, i.e. section "All"). `scope_label` is the section
    /// name surfaced in the level title (empty for "All").
    Search {
        query: String,
        item_types: Vec<String>,
        scope_label: String,
    },
    /// User-requested queue navigation (`n` / `p`).
    QueueNext,
    QueuePrev,
    /// Persist queue prefs (repeat mode + shuffle) to disk.
    SaveAudioPrefs {
        repeat_mode: RepeatMode,
        shuffle: bool,
    },
    /// Persist the user's latest volume choice to disk.
    SaveVolume(u8),
    /// Persist the per-library last-active section map to disk.
    SaveSectionMemory(std::collections::HashMap<String, String>),
    /// Persist the id of the library the user just switched to.
    SaveLastLibrary(String),
    /// Persist the (capped) recent-search-query list.
    SaveSearchHistory(Vec<String>),
    /// Manually fetch detail (cast, lyrics, children, siblings) for the named
    /// item. Triggered by the item-options popup; no longer auto-fetched.
    LoadCurrentDetail { item_id: String, kind: Option<String> },
    /// Re-fetch libraries + their top-level items from the server.
    SyncLibraries,
    /// Fetch every library the server exposes (not just the visible subset) so
    /// the visible-library picker can render its checklist.
    LoadAllLibraryMeta,
    /// Persist the chosen visible-library set + re-sync.
    SaveVisibleLibraries(Vec<String>),
    /// Fetch the user's playlists for the playlist picker; `target_item_id`
    /// rides along so the result knows which item the user wants to add.
    LoadPlaylists { target_item_id: String },
    /// Append `item_id` to `playlist_id`.
    AddToPlaylist { playlist_id: String, item_id: String },
    /// Replace the queue with an Instant Mix seeded from `item` and play it.
    InstantMix { item: Item },
    /// Record a dislike (`POST /UserItems/{id}/Rating?likes=false`).
    Dislike { item_id: String },
    /// Resolve `item_id` to its web URL and copy it to the system clipboard.
    /// `item_name` is shown in the status message.
    CopyItemUrl { item_id: String, item_name: String },
    /// Play `item`, which is already at `app.queue_index` in `app.queue` —
    /// used by Instant Mix so the controller doesn't rebuild the queue from
    /// the active level.
    PlayQueueCurrent { item: Item },
    /// Fetch audio + subtitle stream lists for the video item, then populate
    /// the matching picker overlay.
    LoadVideoTracks { item_id: String, kind: VideoTrackKind },
    /// Fetch versions + audio + subtitle streams for the media-options view
    /// (pre-play config rendered in the content pane).
    LoadMediaOptions { item_id: String },
    /// Launch mpv on an external trailer URL.
    WatchTrailer { url: String, title: String },
    /// Fetch the Home dashboard payload (resume + next-up + per-library
    /// recents). Driven on startup and on manual sync.
    LoadHome,
    /// Run a server-wide search across every library (the `g s` view). The
    /// scoped, library-only search uses `Intent::Search` instead.
    GlobalSearch { query: String },
}

/// Which popup menu, if any, is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupMenu {
    /// Item options for the focused list item (key: `p`).
    Item(usize),
    /// Client settings (key: `Shift+P`).
    Client(usize),
    /// Sleep-timer preset picker (opened from the item menu).
    SleepTimer(usize),
}

/// Entries in the per-item actions popup. The popup is built dynamically per
/// item so audio-only / folder-only actions stay out of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemMenuAction {
    LoadInfo,
    ShowLyrics,
    Play,
    ToggleFavorite,
    TogglePlayed,
    OpenSleepTimerPicker,
    BrowseByGenre,
    BrowseByPerson,
    /// Drill into the parent album of the focused track.
    GoToAlbum,
    /// Drill into the primary artist of the focused track or album.
    GoToArtist,
    PlayNext,
    AddToQueue,
    SaveQueueAsPlaylist,
    GenreRadio,
    WatchTrailer,
    AddToPlaylist,
    InstantMix,
    Dislike,
    ToggleShuffle,
    CycleRepeat,
    AudioTrack,
    Subtitles,
    CopyUrl,
}

/// Which video track stream the picker is listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoTrackKind {
    Audio,
    Subtitle,
}

/// One entry shown in the video track picker.
#[derive(Debug, Clone)]
pub struct TrackPickerEntry {
    pub label: String,
    pub choice: TrackChoice,
}

/// State for the video audio / subtitle picker overlay.
#[derive(Debug, Clone)]
pub struct VideoTrackPickerState {
    pub item_id: String,
    pub item_name: String,
    pub kind: VideoTrackKind,
    pub entries: Option<Vec<TrackPickerEntry>>,
    pub selected: usize,
}

/// Per-item audio + subtitle selection persisted across play presses.
#[derive(Debug, Clone)]
pub struct VideoTrackSelection {
    pub audio: TrackChoice,
    pub subtitle: TrackChoice,
    /// Jellyfin `MediaSourceId` (`None` ⇒ default = item id).
    pub media_source_id: Option<String>,
}

impl Default for VideoTrackSelection {
    fn default() -> Self {
        Self {
            audio: TrackChoice::Auto,
            subtitle: TrackChoice::Auto,
            media_source_id: None,
        }
    }
}

/// One playable version exposed by Jellyfin (multiple per item for files like
/// `Movie (1080p)` + `Movie (4K)`).
#[derive(Debug, Clone)]
pub struct MediaVersion {
    pub source_id: String,
    pub label: String,
}

/// Cursor position inside the media-options view, used so Enter on the right
/// row commits the right selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaOptionsCursor {
    Version(usize),
    Audio(usize),
    Subtitle(usize),
    Chapter(usize),
    Play,
    WatchTrailer,
}

/// State for the content-pane media-options view (versions + audio + subs +
/// Play). Populated by `Intent::LoadMediaOptions`.
#[derive(Debug, Clone)]
pub struct MediaOptionsViewState {
    pub item_id: String,
    pub item_name: String,
    pub loading: bool,
    pub versions: Vec<MediaVersion>,
    pub audio_entries: Vec<TrackPickerEntry>,
    pub subtitle_entries: Vec<TrackPickerEntry>,
    pub selected_version: usize,
    pub selected_audio: usize,
    pub selected_subtitle: usize,
    pub cursor: MediaOptionsCursor,
    /// External trailer URLs (mirrored from `ItemDetail.trailer_urls` once the
    /// detail fetch lands). Watch trailer row is shown when non-empty.
    pub trailer_urls: Vec<String>,
    /// Chapter markers (mirrored from `ItemDetail.chapters`). Pressing Enter on
    /// a chapter row launches mpv with `--start=<seconds>`.
    pub chapters: Vec<Chapter>,
}

/// Which view the music top-right context pane is rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextTopView {
    #[default]
    Lyrics,
    /// Item info: cover, description, contents list (artist's albums, etc).
    Info,
}

/// Entries in the client-settings popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientMenuAction {
    Themes,
    SyncNow,
    VisibleLibraries,
    ToggleGapless,
    ToggleNormalization,
    CycleEqPreset,
    ShowMpvArgs,
    CycleSort,
    ToggleUnplayed,
    ClearFilters,
    Quit,
}

/// State for the visible-library picker overlay. `entries` is the full set of
/// libraries exposed by the server (each tagged with its current visibility);
/// the user toggles entries with Space and confirms with Enter.
#[derive(Debug, Clone)]
pub struct LibraryPickerState {
    pub entries: Vec<(String, String, bool)>,
    pub selected: usize,
    pub loading: bool,
}

/// State for the playlist picker overlay (item-options → Add to playlist).
/// `entries = None` while the async fetch is in flight.
#[derive(Debug, Clone)]
pub struct PlaylistPickerState {
    pub target_item_id: String,
    pub target_item_name: String,
    pub entries: Option<Vec<(String, String)>>,
    pub selected: usize,
}

/// Display-only snapshot of the active playback, written by the event loop each
/// tick and read by the now-playing renderer.
#[derive(Debug, Clone)]
pub struct NowPlaying {
    /// Item id of what's playing, so the cover can be fetched/shown.
    pub item_id: String,
    pub kind: MediaKind,
    pub title: String,
    pub subtitle: Option<String>,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub paused: bool,
    /// Audio only; mpv owns its own volume.
    pub volume: Option<u8>,
}

impl Item {
    /// A title-only item, for demo/mock data and tests.
    pub fn demo(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }
}

/// A library (top-bar chip) and its top-level items.
#[derive(Debug, Clone)]
pub struct Library {
    /// Jellyfin view id (used as the parent id of its top-level items).
    pub id: String,
    pub name: String,
    /// Jellyfin `CollectionType` (e.g. `music`, `movies`, `tvshows`). Drives the
    /// right column's context (lyrics+queue vs cast+credits vs episodes+seasons).
    pub collection_type: Option<String>,
    pub items: Vec<Item>,
}

/// A sub-view of a library — e.g. for music: Albums, Album Artists, Songs.
/// Drives the items query that fills the library_items pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    /// `includeItemTypes` filter (empty = top-level items, no type filter).
    pub item_types: Vec<String>,
    /// `sortBy` keys; defaults to SortName when empty.
    pub sort_by: Vec<String>,
}

impl Section {
    fn new(name: &str, item_types: &[&str], sort_by: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            item_types: item_types.iter().map(|s| s.to_string()).collect(),
            sort_by: sort_by.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Runtime filter + sort overrides applied on top of a [`Section`]. Empty
/// fields are no-ops; set fields merge into the items query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionFilters {
    pub genres: Vec<String>,
    pub person_ids: Vec<String>,
    pub years: Vec<i32>,
    pub studio_ids: Vec<String>,
    pub tags: Vec<String>,
    /// Jellyfin `Filters` (e.g. `IsUnplayed`, `IsFavorite`).
    pub filters: Vec<String>,
    /// When set, overrides the section's `sort_by` order.
    pub sort_override: Option<Vec<String>>,
}

/// Cast / crew member shown in the right-column context pane for movies/tv.
#[derive(Debug, Clone, Default)]
pub struct Person {
    /// Jellyfin person id (used to filter items via `personIds=`).
    pub id: Option<String>,
    pub name: String,
    /// e.g. `Neo`, `Director`, `Writer`. May be empty.
    pub role: Option<String>,
    /// Jellyfin `Type` (`Actor`, `Director`, `Writer`, `GuestStar`, …).
    pub kind: Option<String>,
}

/// Sleep timer choice. Cycles via the item-menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SleepTimer {
    #[default]
    Off,
    In5,
    In10,
    In15,
    In20,
    In30,
    In45,
    In60,
    In90,
    In120,
    /// Stop after the current track finishes (audio) or current item ends (video).
    EndOfTrack,
}

impl SleepTimer {
    /// Full preset list in picker-display order. `Off` first so it doubles as
    /// "Cancel" when the timer is already armed.
    pub const PRESETS: &'static [SleepTimer] = &[
        SleepTimer::Off,
        SleepTimer::In5,
        SleepTimer::In10,
        SleepTimer::In15,
        SleepTimer::In20,
        SleepTimer::In30,
        SleepTimer::In45,
        SleepTimer::In60,
        SleepTimer::In90,
        SleepTimer::In120,
        SleepTimer::EndOfTrack,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SleepTimer::Off => "Off",
            SleepTimer::In5 => "5 min",
            SleepTimer::In10 => "10 min",
            SleepTimer::In15 => "15 min",
            SleepTimer::In20 => "20 min",
            SleepTimer::In30 => "30 min",
            SleepTimer::In45 => "45 min",
            SleepTimer::In60 => "1 hour",
            SleepTimer::In90 => "1h 30m",
            SleepTimer::In120 => "2 hours",
            SleepTimer::EndOfTrack => "End of track",
        }
    }

    /// `Some(duration)` for absolute-time choices, `None` for Off / EndOfTrack.
    pub fn duration(self) -> Option<std::time::Duration> {
        let mins = match self {
            SleepTimer::In5 => 5,
            SleepTimer::In10 => 10,
            SleepTimer::In15 => 15,
            SleepTimer::In20 => 20,
            SleepTimer::In30 => 30,
            SleepTimer::In45 => 45,
            SleepTimer::In60 => 60,
            SleepTimer::In90 => 90,
            SleepTimer::In120 => 120,
            SleepTimer::Off | SleepTimer::EndOfTrack => return None,
        };
        Some(std::time::Duration::from_secs(mins * 60))
    }
}

/// Queue repeat mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    /// Stop after the last track.
    #[default]
    Off,
    /// Loop the entire queue back to the start.
    All,
    /// Loop the current track forever.
    One,
}

impl RepeatMode {
    /// `r` cycles Off → All → One → Off.
    pub fn cycle(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RepeatMode::Off => "Off",
            RepeatMode::All => "All",
            RepeatMode::One => "One",
        }
    }
}

impl From<crate::config::RepeatModePref> for RepeatMode {
    fn from(p: crate::config::RepeatModePref) -> Self {
        match p {
            crate::config::RepeatModePref::Off => RepeatMode::Off,
            crate::config::RepeatModePref::All => RepeatMode::All,
            crate::config::RepeatModePref::One => RepeatMode::One,
        }
    }
}

impl From<RepeatMode> for crate::config::RepeatModePref {
    fn from(m: RepeatMode) -> Self {
        match m {
            RepeatMode::Off => crate::config::RepeatModePref::Off,
            RepeatMode::All => crate::config::RepeatModePref::All,
            RepeatMode::One => crate::config::RepeatModePref::One,
        }
    }
}

/// One line of lyrics, optionally timestamped for synced display.
#[derive(Debug, Clone, Default)]
pub struct LyricLine {
    pub text: String,
    /// Start time in 100 ns ticks; absent on plain-text lyrics.
    pub start_ticks: Option<i64>,
}

/// Fetched detail for the currently-selected item.
#[derive(Debug, Clone, Default)]
pub struct ItemDetail {
    /// The selected item's own metadata (Overview, name, etc) — re-fetched so
    /// the info pane has a description even when the level row was sparse.
    pub overview: Option<String>,
    /// Cast and crew (movies + tv).
    pub cast: Vec<Person>,
    pub genres: Vec<String>,
    /// Lyrics lines (audio items only). `None` means none fetched yet,
    /// `Some(empty)` means the server has no lyrics for this track.
    pub lyrics: Option<Vec<LyricLine>>,
    /// Immediate children of the selected item (TV series → seasons; season →
    /// episodes; etc). Empty for non-container items.
    pub children: Vec<Item>,
    /// Siblings of the selected item — items sharing its parent. Used so the
    /// TV context can show season-mates from a focused episode.
    pub siblings: Vec<Item>,
    /// Albums credited to this artist (MusicArtist items only).
    pub artist_albums: Vec<Item>,
    /// Albums the artist contributes to but isn't the primary album-artist of.
    pub appears_on: Vec<Item>,
    /// External trailer URLs (YouTube etc.) for the item.
    pub trailer_urls: Vec<String>,
    /// Chapter markers from `BaseItemDto.Chapters`. Empty when the server
    /// returned none (or the field was not requested).
    pub chapters: Vec<Chapter>,
}

/// One chapter marker on a video item (name + position into the file).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chapter {
    pub name: String,
    /// 100 ns Jellyfin ticks into the item.
    pub start_position_ticks: i64,
}

/// In-place Fisher-Yates shuffle backed by a tiny linear-congruential PRNG
/// seeded from `SystemTime`. Not cryptographic — fine for a play queue.
fn shuffle_in_place<T>(items: &mut [T]) {
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    let len = items.len();
    for i in (1..len).rev() {
        // Numerical Recipes LCG; cheap and good enough for picking indices.
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let j = (state >> 33) as usize % (i + 1);
        items.swap(i, j);
    }
}

/// Static section list for a Jellyfin collection type. The first entry ("All")
/// is the default that matches the library root.
pub fn sections_for(collection_type: Option<&str>) -> Vec<Section> {
    match collection_type {
        Some("music") => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Albums", &["MusicAlbum"], &["SortName"]),
            Section::new("Album Artists", &["MusicArtist"], &["SortName"]),
            Section::new("Songs", &["Audio"], &["SortName"]),
            Section::new("Playlists", &["Playlist"], &["SortName"]),
        ],
        Some("movies") => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Latest", &["Movie"], &["DateCreated"]),
            Section::new("Collections", &["BoxSet"], &["SortName"]),
            Section::new("Favorites", &["Movie"], &["SortName"]),
            Section::new("Playlists", &["Playlist"], &["SortName"]),
        ],
        Some("tvshows") => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Series", &["Series"], &["SortName"]),
            Section::new("Episodes", &["Episode"], &["DateCreated"]),
            Section::new("Playlists", &["Playlist"], &["SortName"]),
        ],
        Some("books") => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Books", &["Book"], &["SortName"]),
            Section::new("Authors", &["Person"], &["SortName"]),
            Section::new("Playlists", &["Playlist"], &["SortName"]),
        ],
        Some("photos") => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Albums", &["PhotoAlbum"], &["SortName"]),
            Section::new("Photos", &["Photo"], &["DateCreated"]),
            Section::new("Playlists", &["Playlist"], &["SortName"]),
        ],
        _ => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Playlists", &["Playlist"], &["SortName"]),
        ],
    }
}

/// One level of the list pane's drill-down stack. The bottom level holds the
/// selected library's top-level items; deeper levels hold a folder's children.
#[derive(Debug, Clone)]
pub struct Level {
    /// Breadcrumb label (library or folder name).
    pub title: String,
    /// Id of the folder/library whose children these are. Identifies the level
    /// when an async children-load completes.
    pub parent_id: String,
    pub items: Vec<Item>,
    pub selected: usize,
    /// Children are still being fetched.
    pub loading: bool,
    /// The item the user drilled into to open this level (the album, artist,
    /// series, …). Used so the info pane can render the parent while the
    /// children are still loading. `None` for the library root.
    pub parent_item: Option<Item>,
    /// `false` on a freshly drilled level: the user hasn't moved the cursor
    /// yet, so the info pane should stay on `parent_item` even after the
    /// children have loaded. The first Up/Down/Top/Bottom flips this to
    /// `true` so subsequent renders follow the selected child.
    pub cursor_engaged: bool,
}

impl Level {
    /// A ready-to-show level (used for library roots and for filled folders).
    fn ready(title: impl Into<String>, parent_id: impl Into<String>, items: Vec<Item>) -> Self {
        Self {
            title: title.into(),
            parent_id: parent_id.into(),
            items,
            selected: 0,
            loading: false,
            parent_item: None,
            cursor_engaged: true,
        }
    }
}

/// Build the list pane's bottom level from a library (empty when there are none).
fn root_level(library: Option<&Library>) -> Vec<Level> {
    match library {
        Some(library) => vec![Level::ready(
            library.name.clone(),
            library.id.clone(),
            library.items.clone(),
        )],
        None => Vec::new(),
    }
}

/// All TUI state. Pure: [`App::handle_key`] is a state transition with no I/O,
/// which keeps it unit-testable without a real terminal.
#[derive(Debug)]
pub struct App {
    pub focus: Pane,
    pub libraries: Vec<Library>,
    /// Index of the active library (selected via the top bar's 1-9 keys).
    pub library_selected: usize,
    /// Active section index within the current library's [`sections_for`] list.
    /// Reset to 0 (the "All" section) on library change.
    pub section_selected: usize,
    /// The list pane's drill-down stack. Never empty while a library exists:
    /// `stack[0]` is the selected library's top-level items.
    pub stack: Vec<Level>,
    /// `Some` when the search input is focused; the string is the in-progress
    /// query. Cleared on Esc or when the user navigates away.
    pub search_query: Option<String>,
    /// Fetched detail for the current selection (`(id, detail)`). Cleared on
    /// selection change so the renderer doesn't show stale cast/lyrics.
    current_detail: Option<(String, ItemDetail)>,
    /// Audio play queue. Populated on Audio Play with the sibling audio items
    /// from the active level; advanced when the engine reports the current
    /// track finished.
    pub queue: Vec<Item>,
    /// Index of the currently-playing track within `queue`, or `None` when no
    /// audio is playing.
    pub queue_index: Option<usize>,
    pub repeat_mode: RepeatMode,
    /// Current sleep-timer choice. Cycled via the item-menu entry; the playback
    /// controller arms a real deadline when this changes.
    pub sleep_timer: SleepTimer,
    /// Seconds left on the timed sleep choice. Written each tick by the playback
    /// controller and shown in the sleep-timer entry/picker. `None` for Off and
    /// for `EndOfTrack` (which has no fixed deadline).
    pub sleep_remaining_secs: Option<u64>,
    /// Per-library runtime filter/sort overrides (cleared by toggling them off
    /// in the client menu). Merged into the items query on each `ApplySection`.
    pub section_filters: std::collections::HashMap<String, SectionFilters>,
    /// Gapless audio toggle (mirrors `config.audio.gapless`).
    pub gapless: bool,
    /// ReplayGain normalization toggle (mirrors `config.audio.normalization`).
    pub normalization: bool,
    /// Current EQ preset (mirrors `config.audio.eq.preset`; `Flat` == disabled).
    pub eq_preset: crate::config::EqPreset,
    /// Number of custom mpv args from `config.video.mpv_args`. Display-only
    /// here — editing happens by hand in `config.toml`.
    pub mpv_arg_count: usize,
    /// True when shuffle is on. The queue list is reordered in place each time
    /// shuffle flips on (so the auto-advance pointer follows the new order).
    pub shuffle: bool,
    /// Last-active section *name* per library id. Stored by name rather than
    /// index so a future schema change to [`sections_for`] won't strand users
    /// on a stale slot. Lives in-memory and persists to disk via
    /// `Intent::SaveSectionMemory`.
    section_memory: std::collections::HashMap<String, String>,
    /// Recent search queries (most recent first). Up/Down inside the search
    /// input cycles through them so the user can re-run a recent search.
    search_history: Vec<String>,
    /// Index of the in-history query currently surfaced in the search input,
    /// or `None` when the user is typing fresh.
    search_history_cursor: Option<usize>,
    pub show_help: bool,
    pub should_quit: bool,
    /// Display snapshot of current playback; set by the event loop, read by render.
    pub now_playing: Option<NowPlaying>,
    /// The active color theme.
    pub theme: Theme,
    /// Selectable theme names, for the runtime picker.
    available_themes: Vec<String>,
    /// When the theme picker is open, the highlighted index into `available_themes`.
    theme_picker: Option<usize>,
    /// Currently-open popup menu (item options or client settings), if any.
    popup: Option<PopupMenu>,
    /// True while the user has pressed `g` and the next keystroke should be
    /// resolved by the go-to menu instead of the normal keymap.
    awaiting_go_to: bool,
    /// Visible-library picker overlay (Client settings → Visible libraries…).
    library_picker: Option<LibraryPickerState>,
    /// Playlist picker overlay (Item options → Add to playlist).
    playlist_picker: Option<PlaylistPickerState>,
    /// Video audio / subtitle track picker overlay.
    video_track_picker: Option<VideoTrackPickerState>,
    /// Per-item video track selections; consumed by [`Playback`] on Play.
    video_track_selections: std::collections::HashMap<String, VideoTrackSelection>,
    /// Pre-play media-options view rendered in the content pane for video
    /// items (versions + audio + subtitles + Play).
    media_options_view: Option<MediaOptionsViewState>,
    /// Id of the item the user has explicitly revealed via the popup's "Load
    /// info" action. While `None`, the middle pane shows a placeholder instead
    /// of cover + title + description. Cleared when selection changes.
    revealed_item_id: Option<String>,
    /// Which view the music top-right pane renders (info or lyrics). Toggled
    /// from the item-options popup.
    context_view: ContextTopView,
    /// Vertical scroll offset (in rows) for the right-top context pane when
    /// it overflows. Reset on selection change or view switch.
    context_top_scroll: u16,
    /// Vertical scroll offset (in rows) for the right-bottom context pane
    /// (queue / credits / seasons).
    context_bottom_scroll: u16,
    /// Transient one-liner in the status bar (e.g. "Not playable"); cleared on the
    /// next key press.
    status_message: Option<String>,
    /// Side effects queued by [`App::handle_key`] for the loop to perform.
    pending: Vec<Intent>,
    error: Option<String>,
    error_copied: bool,
    keymap: Keymap,
    /// Home dashboard payload (resume + next-up + recents per library).
    home: HomeData,
    /// Row/col cursor on Home. Lives separately from `focus` so leaving and
    /// returning to Home preserves the user's spot.
    home_cursor: HomeCursor,
    /// Global search view state. Lives in App so the query + last results
    /// survive leaving and re-entering the view.
    global_search: GlobalSearchState,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Demo/mock data; used by tests and as a fallback. Real data comes via
    /// [`App::with_libraries`].
    pub fn new() -> Self {
        Self::with_libraries(vec![
            Library {
                id: "movies".to_string(),
                name: "Movies".to_string(),
                collection_type: Some("movies".to_string()),
                items: vec![
                    Item::demo("The Matrix"),
                    Item::demo("Inception"),
                    Item::demo("Blade Runner 2049"),
                    Item::demo("Arrival"),
                    Item::demo("Dune"),
                ],
            },
            Library {
                id: "tv".to_string(),
                name: "TV".to_string(),
                collection_type: Some("tvshows".to_string()),
                items: vec![
                    Item::demo("Severance"),
                    Item::demo("The Bear"),
                    Item::demo("Andor"),
                    Item::demo("Breaking Bad"),
                ],
            },
            Library {
                id: "music".to_string(),
                name: "Music".to_string(),
                collection_type: Some("music".to_string()),
                items: vec![
                    Item::demo("Discovery — Daft Punk"),
                    Item::demo("In Rainbows — Radiohead"),
                    Item::demo("Random Access Memories"),
                ],
            },
        ])
    }

    pub fn with_libraries(libraries: Vec<Library>) -> Self {
        let stack = root_level(libraries.first());
        Self {
            focus: Pane::LibraryItems,
            libraries,
            library_selected: 0,
            section_selected: 0,
            stack,
            search_query: None,
            current_detail: None,
            queue: Vec::new(),
            queue_index: None,
            repeat_mode: RepeatMode::Off,
            sleep_timer: SleepTimer::Off,
            sleep_remaining_secs: None,
            section_filters: std::collections::HashMap::new(),
            gapless: false,
            normalization: false,
            eq_preset: crate::config::EqPreset::Flat,
            mpv_arg_count: 0,
            shuffle: false,
            section_memory: std::collections::HashMap::new(),
            search_history: Vec::new(),
            search_history_cursor: None,
            show_help: false,
            should_quit: false,
            now_playing: None,
            theme: Theme::default(),
            available_themes: Vec::new(),
            theme_picker: None,
            popup: None,
            awaiting_go_to: false,
            library_picker: None,
            playlist_picker: None,
            video_track_picker: None,
            video_track_selections: std::collections::HashMap::new(),
            media_options_view: None,
            revealed_item_id: None,
            context_view: ContextTopView::default(),
            context_top_scroll: 0,
            context_bottom_scroll: 0,
            status_message: None,
            pending: Vec::new(),
            error: None,
            error_copied: false,
            keymap: Keymap::default(),
            home: HomeData::default(),
            home_cursor: HomeCursor::default(),
            global_search: GlobalSearchState {
                input_focused: true,
                ..Default::default()
            },
        }
    }

    /// Open the global search view. Lifts every overlay; the input regains
    /// focus so the user can type immediately.
    pub fn enter_global_search(&mut self) {
        self.focus = Pane::GlobalSearch;
        self.popup = None;
        self.library_picker = None;
        self.playlist_picker = None;
        self.video_track_picker = None;
        self.media_options_view = None;
        self.theme_picker = None;
        self.show_help = false;
        self.global_search.input_focused = true;
        self.status_message = None;
    }

    pub fn is_on_global_search(&self) -> bool {
        self.focus == Pane::GlobalSearch
    }

    pub fn global_search_state(&self) -> &GlobalSearchState {
        &self.global_search
    }

    /// Populate the global-search result list (called by Browser on completion).
    pub fn set_global_search_results(&mut self, query: &str, results: Vec<Item>) {
        if self.global_search.query.trim() != query {
            return;
        }
        self.global_search.results = results;
        self.global_search.loading = false;
        self.global_search.submitted = true;
        self.global_search.selected = 0;
        // After results land, drop focus into the list so Up/Down navigate.
        if !self.global_search.results.is_empty() {
            self.global_search.input_focused = false;
        }
    }

    fn submit_global_search(&mut self) {
        let q = self.global_search.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.global_search.loading = true;
        self.global_search.results.clear();
        self.pending.push(Intent::GlobalSearch { query: q });
    }

    fn handle_global_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                // Leave the view; return to Home unless the user came from a
                // library, in which case fall back to the items pane.
                self.focus = if self.libraries.is_empty() {
                    Pane::Home
                } else {
                    Pane::LibraryItems
                };
            }
            KeyCode::Enter => {
                if self.global_search.input_focused {
                    self.submit_global_search();
                } else if let Some(item) = self
                    .global_search
                    .results
                    .get(self.global_search.selected)
                    .cloned()
                {
                    self.activate_global_search_item(item);
                }
            }
            KeyCode::Up => {
                if self.global_search.input_focused {
                    return;
                }
                if self.global_search.selected == 0 {
                    self.global_search.input_focused = true;
                } else {
                    self.global_search.selected -= 1;
                }
            }
            KeyCode::Down => {
                if self.global_search.input_focused {
                    if !self.global_search.results.is_empty() {
                        self.global_search.input_focused = false;
                    }
                    return;
                }
                let max = self.global_search.results.len().saturating_sub(1);
                if self.global_search.selected < max {
                    self.global_search.selected += 1;
                }
            }
            KeyCode::Tab => {
                self.global_search.input_focused = !self.global_search.input_focused;
            }
            KeyCode::Backspace => {
                if self.global_search.input_focused {
                    self.global_search.query.pop();
                }
            }
            KeyCode::Char(c) if self.global_search.input_focused
                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.global_search.query.push(c);
            }
            _ => {}
        }
    }

    fn activate_global_search_item(&mut self, item: Item) {
        if item.is_folder {
            self.focus = Pane::LibraryItems;
            self.drill_into(&item);
            return;
        }
        match MediaKind::classify(item.kind.as_deref()) {
            MediaKind::Video => {
                self.focus = Pane::LibraryItems;
                self.open_media_options_view(item);
            }
            MediaKind::Audio => {
                self.focus = Pane::LibraryItems;
                self.pending.push(Intent::Play {
                    item,
                    media: MediaKind::Audio,
                    start_ticks: None,
                });
            }
            MediaKind::Other => {
                self.status_message = Some(format!("Not playable: {}", item.name));
            }
        }
    }

    /// Focus the Home dashboard. Used at startup and when `g h` is pressed.
    /// Also kicks a `LoadHome` so the loop refreshes the payload.
    pub fn enter_home(&mut self) {
        self.focus = Pane::Home;
        self.popup = None;
        self.library_picker = None;
        self.playlist_picker = None;
        self.video_track_picker = None;
        self.media_options_view = None;
        self.theme_picker = None;
        self.show_help = false;
        if !self.home.loading && self.home.resume.is_empty()
            && self.home.next_up.is_empty()
            && self.home.recent_per_library.is_empty()
        {
            self.home.loading = true;
            self.pending.push(Intent::LoadHome);
        }
    }

    /// Start the app on Home and queue the first dashboard fetch.
    pub fn with_home_start(mut self) -> Self {
        self.focus = Pane::Home;
        self.home.loading = true;
        self.pending.push(Intent::LoadHome);
        self
    }

    pub fn is_on_home(&self) -> bool {
        self.focus == Pane::Home
    }

    pub fn home_data(&self) -> &HomeData {
        &self.home
    }

    pub fn home_cursor(&self) -> HomeCursor {
        self.home_cursor
    }

    /// Build the visible Home rows. Rows with no items are dropped so the
    /// cursor can never land on emptiness. `Libraries` is always shown so the
    /// user has a non-empty target on a fresh install.
    pub fn home_rows(&self) -> Vec<HomeRow> {
        let mut rows: Vec<HomeRow> = Vec::new();
        if !self.home.resume.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::Resume,
                title: "Continue".to_string(),
                library_id: None,
                items: self.home.resume.clone(),
            });
        }
        if !self.home.next_up.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::NextUp,
                title: "Next Up".to_string(),
                library_id: None,
                items: self.home.next_up.clone(),
            });
        }
        if !self.home.latest_global.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::Recent,
                title: "Latest".to_string(),
                library_id: None,
                items: self.home.latest_global.clone(),
            });
        }
        // Libraries row: synthesised from the library list itself, so each
        // library appears as a tile whether or not we have any "recent" art.
        let library_tiles: Vec<Item> = self
            .libraries
            .iter()
            .map(|lib| Item {
                id: lib.id.clone(),
                name: lib.name.clone(),
                is_folder: true,
                // Carry the collection type as the item's kind so the Home
                // renderer can pick a per-library glyph (▣ for movies, ♪ for
                // music, …). MediaKind::classify still returns Other so
                // activation routes through the Libraries-row branch.
                kind: lib.collection_type.clone(),
                primary_image_tag: self
                    .home
                    .recent_per_library
                    .iter()
                    .find(|s| s.id == lib.id)
                    .and_then(|s| s.recent.first())
                    .and_then(|i| i.primary_image_tag.clone()),
                ..Default::default()
            })
            .collect();
        if !library_tiles.is_empty() {
            rows.push(HomeRow {
                kind: HomeRowKind::Libraries,
                title: "Libraries".to_string(),
                library_id: None,
                items: library_tiles,
            });
        }
        for summary in &self.home.recent_per_library {
            if summary.recent.is_empty() {
                continue;
            }
            rows.push(HomeRow {
                kind: HomeRowKind::Recent,
                title: format!("New in {}", summary.name),
                library_id: Some(summary.id.clone()),
                items: summary.recent.clone(),
            });
        }
        rows
    }

    /// Replace the home payload (called by the Browser when fetch completes).
    pub fn set_home_data(&mut self, data: HomeData) {
        self.home = data;
        self.home.loading = false;
        let rows = self.home_rows();
        if self.home_cursor.row >= rows.len() {
            self.home_cursor = HomeCursor::default();
        } else if let Some(row) = rows.get(self.home_cursor.row) {
            if self.home_cursor.col >= row.items.len() {
                self.home_cursor.col = row.items.len().saturating_sub(1);
            }
        }
    }

    fn move_home_cursor(&mut self, drow: i32, dcol: i32) {
        let rows = self.home_rows();
        if rows.is_empty() {
            return;
        }
        if drow != 0 {
            let next = (self.home_cursor.row as i32 + drow)
                .clamp(0, rows.len() as i32 - 1) as usize;
            self.home_cursor.row = next;
            // Clamp column into the new row.
            let row_len = rows[next].items.len().max(1);
            if self.home_cursor.col >= row_len {
                self.home_cursor.col = row_len - 1;
            }
        }
        if dcol != 0 {
            let row_len = rows[self.home_cursor.row].items.len();
            if row_len == 0 {
                return;
            }
            let next = (self.home_cursor.col as i32 + dcol)
                .clamp(0, row_len as i32 - 1) as usize;
            self.home_cursor.col = next;
        }
    }

    fn home_top(&mut self) {
        let rows = self.home_rows();
        if rows.is_empty() {
            return;
        }
        self.home_cursor.col = 0;
    }

    fn home_bottom(&mut self) {
        let rows = self.home_rows();
        if let Some(row) = rows.get(self.home_cursor.row) {
            if !row.items.is_empty() {
                self.home_cursor.col = row.items.len() - 1;
            }
        }
    }

    /// Resolve the focused Home tile. Returns `(row_kind, library_id, item)`.
    pub fn home_focused(&self) -> Option<(HomeRowKind, Option<String>, Item)> {
        let rows = self.home_rows();
        let row = rows.get(self.home_cursor.row)?;
        let item = row.items.get(self.home_cursor.col)?.clone();
        Some((row.kind, row.library_id.clone(), item))
    }

    /// Enter on a Home tile: library tiles jump to that library (and leave
    /// Home), other tiles play / drill the item.
    fn activate_home(&mut self) {
        let Some((kind, _library_id, item)) = self.home_focused() else {
            return;
        };
        match kind {
            HomeRowKind::Libraries => {
                if let Some(index) = self.libraries.iter().position(|l| l.id == item.id) {
                    self.library_selected = index;
                    self.reset_stack_for_library();
                    self.focus = Pane::LibraryItems;
                    if let Some(library) = self.current_library() {
                        let id = library.id.clone();
                        self.pending.push(Intent::SaveLastLibrary(id));
                    }
                }
            }
            _ => {
                if item.is_folder {
                    // Drill into a series/album/etc. Find the library it
                    // belongs to so the right side panes have context.
                    self.focus = Pane::LibraryItems;
                    self.drill_into(&item);
                    return;
                }
                match MediaKind::classify(item.kind.as_deref()) {
                    MediaKind::Video => {
                        self.focus = Pane::LibraryItems;
                        self.open_media_options_view(item);
                    }
                    MediaKind::Audio => {
                        self.focus = Pane::LibraryItems;
                        self.pending.push(Intent::Play {
                            item,
                            media: MediaKind::Audio,
                            start_ticks: None,
                        });
                    }
                    MediaKind::Other => {
                        self.status_message =
                            Some(format!("Not playable: {}", item.name));
                    }
                }
            }
        }
    }

    /// Replace the keymap (built from config at startup).
    pub fn with_keymap(mut self, keymap: Keymap) -> Self {
        self.keymap = keymap;
        self
    }

    /// Set the active theme (startup, from config, or a runtime switch).
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Provide the selectable theme names (built-ins + user themes).
    pub fn with_available_themes(mut self, names: Vec<String>) -> Self {
        self.available_themes = names;
        self
    }

    /// Seed the queue mode + shuffle from persisted config.
    /// Wire the persisted audio-feature toggles (gapless / normalization /
    /// EQ preset) and the mpv-args count so the client menu reflects them.
    pub fn with_audio_features(
        mut self,
        gapless: bool,
        normalization: bool,
        eq_preset: crate::config::EqPreset,
        mpv_arg_count: usize,
    ) -> Self {
        self.gapless = gapless;
        self.normalization = normalization;
        self.eq_preset = eq_preset;
        self.mpv_arg_count = mpv_arg_count;
        self
    }

    pub fn with_audio_prefs(mut self, repeat_mode: RepeatMode, shuffle: bool) -> Self {
        self.repeat_mode = repeat_mode;
        self.shuffle = shuffle;
        self
    }

    /// Seed the per-library section memory from persisted config.
    pub fn with_section_memory(
        mut self,
        memory: std::collections::HashMap<String, String>,
    ) -> Self {
        self.section_memory = memory;
        // Restore the active library's remembered section so the first render
        // matches the persisted state.
        self.reset_stack_for_library();
        self
    }

    /// Focus the library by Jellyfin id (used at startup to restore the
    /// previously-active library). Falls back to the first library if the id
    /// no longer exists.
    pub fn with_last_library(mut self, id: Option<String>) -> Self {
        if let Some(id) = id {
            if let Some(index) = self.libraries.iter().position(|l| l.id == id) {
                self.library_selected = index;
                self.reset_stack_for_library();
            }
        }
        self
    }

    /// Seed the in-app recent-search list from persisted config.
    pub fn with_search_history(mut self, history: Vec<String>) -> Self {
        self.search_history = history;
        self
    }

    /// Picker overlay state for the renderer: the names and the highlighted index.
    pub fn theme_picker(&self) -> Option<(&[String], usize)> {
        self.theme_picker
            .map(|selected| (self.available_themes.as_slice(), selected))
    }

    /// Surface an error to the user via the modal overlay.
    pub fn show_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
        self.error_copied = false;
    }

    pub fn current_library(&self) -> Option<&Library> {
        self.libraries.get(self.library_selected)
    }

    /// The active list level (top of the drill stack).
    pub fn current_level(&self) -> Option<&Level> {
        self.stack.last()
    }

    fn current_level_mut(&mut self) -> Option<&mut Level> {
        self.stack.last_mut()
    }

    /// The library root level (bottom of the drill stack). Shown in the left
    /// items pane so the album/playlist/series list stays visible while the
    /// middle pane drills into a folder.
    pub fn root_level(&self) -> Option<&Level> {
        self.stack.first()
    }

    /// The deepest drilled level — the middle pane's list when the user has
    /// opened a folder. `None` while at the library root.
    pub fn drilled_level(&self) -> Option<&Level> {
        if self.is_drilled() {
            self.stack.last()
        } else {
            None
        }
    }

    /// Item the user is currently engaging with: the focused row in whichever
    /// pane holds focus. When focus is on the items list (top-left), this is
    /// the library root selection — so drilling into a Series leaves
    /// `current_item` on the Series itself, and the info pane keeps rendering
    /// it. When focus is on the Content pane, this walks into the drilled
    /// level instead — falling back to the level's `parent_item` while its
    /// children are still loading so the info pane never goes blank.
    pub fn current_item(&self) -> Option<&Item> {
        let level = match self.focus {
            Pane::LibraryItems => self.root_level(),
            _ => self.current_level(),
        };
        let level = level?;
        // Until the user moves the cursor in this drilled level, the info pane
        // stays pinned on the drilled-into parent. Otherwise the parent's
        // detail would flash for a moment and then get replaced by the first
        // child's detail as soon as the children load.
        if !level.cursor_engaged {
            if let Some(parent) = level.parent_item.as_ref() {
                return Some(parent);
            }
        }
        level
            .items
            .get(level.selected)
            .or(level.parent_item.as_ref())
    }

    /// Breadcrumb of the current drill path within the library (e.g.
    /// "Breaking Bad › Season 1").
    pub fn breadcrumb(&self) -> String {
        self.stack
            .iter()
            .map(|level| level.title.as_str())
            .collect::<Vec<_>>()
            .join(" › ")
    }

    /// Whether the list pane is showing a folder's children (vs. a library root).
    pub fn is_drilled(&self) -> bool {
        self.stack.len() > 1
    }

    /// Rebuild the stack for the currently-selected library (called whenever the
    /// library selection changes; drilling state is intentionally reset).
    /// Restores any remembered section for the new library, so that flicking
    /// back with 1-9 doesn't drop the user at "All" every time.
    fn reset_stack_for_library(&mut self) {
        self.stack = root_level(self.current_library());
        self.section_selected = self
            .current_library()
            .and_then(|lib| {
                let remembered = self.section_memory.get(&lib.id)?.clone();
                sections_for(lib.collection_type.as_deref())
                    .iter()
                    .position(|s| s.name == remembered)
            })
            .unwrap_or(0);
    }

    /// Sections defined for the active library kind.
    pub fn current_sections(&self) -> Vec<Section> {
        sections_for(
            self.current_library()
                .and_then(|l| l.collection_type.as_deref()),
        )
    }

    /// Currently-focused section (within the active library).
    pub fn current_section(&self) -> Option<Section> {
        self.current_sections().into_iter().nth(self.section_selected)
    }

    /// Detail for the current item, if it has been fetched and is still
    /// current (the item id matches).
    pub fn current_detail(&self) -> Option<&ItemDetail> {
        let id = self.current_item()?.id.as_str();
        self.current_detail
            .as_ref()
            .filter(|(detail_id, _)| detail_id == id)
            .map(|(_, detail)| detail)
    }

    /// Store fetched detail for `item_id` (ignored if the selection has moved
    /// on since the request was queued).
    pub fn set_current_detail(&mut self, item_id: &str, detail: ItemDetail) {
        if let Some(view) = self.media_options_view.as_mut() {
            if view.item_id == item_id {
                view.trailer_urls = detail.trailer_urls.clone();
                view.chapters = detail.chapters.clone();
            }
        }
        if self
            .current_item()
            .is_some_and(|item| item.id == item_id)
        {
            self.current_detail = Some((item_id.to_string(), detail));
        }
    }

    /// Reset the per-selection transient state (scroll offsets, the open
    /// media-options view) without dropping cached detail. Called by the run
    /// loop when the user moves to a different item, so the info pane keeps
    /// its content but the right pane scrolls back to the top.
    pub fn reset_context_scroll_for_selection_change(&mut self) {
        self.context_top_scroll = 0;
        self.context_bottom_scroll = 0;
        self.media_options_view = None;
    }

    /// Drop any cached detail (called when the selection changes so the
    /// renderer doesn't show stale lyrics/cast). Also drops the "revealed"
    /// flag so the middle pane goes back to its placeholder, resets the
    /// context-pane scroll, and snaps the music context back to lyrics.
    pub fn clear_current_detail(&mut self) {
        self.current_detail = None;
        self.revealed_item_id = None;
        self.context_top_scroll = 0;
        self.context_bottom_scroll = 0;
        self.context_view = ContextTopView::default();
        self.media_options_view = None;
    }

    pub fn context_view(&self) -> ContextTopView {
        self.context_view
    }

    pub fn context_top_scroll(&self) -> u16 {
        self.context_top_scroll
    }

    pub fn context_bottom_scroll(&self) -> u16 {
        self.context_bottom_scroll
    }

    pub fn set_context_view(&mut self, view: ContextTopView) {
        if self.context_view != view {
            self.context_view = view;
            self.context_top_scroll = 0;
        }
    }

    /// Scroll the focused context pane (top or bottom). Falls back to the top
    /// pane when neither has focus so PgUp/PgDn always do something useful.
    fn scroll_focused_context(&mut self, delta: i32) {
        let scroll = match self.focus {
            Pane::ContextBottom => &mut self.context_bottom_scroll,
            _ => &mut self.context_top_scroll,
        };
        let next = *scroll as i32 + delta;
        *scroll = next.max(0) as u16;
    }

    /// True when the focused item has been explicitly revealed via the
    /// item-options popup. Drives the middle pane (cover + title + description)
    /// and the cover request gate.
    pub fn current_item_revealed(&self) -> bool {
        match (self.current_item(), &self.revealed_item_id) {
            (Some(item), Some(id)) => item.id == *id,
            _ => false,
        }
    }

    /// Mark `item_id` as revealed (called when the user picks "Load info").
    pub fn reveal_item(&mut self, item_id: String) {
        self.revealed_item_id = Some(item_id);
    }

    /// Build the audio queue from the active level: every audio item in the
    /// current list, with the focused item as the starting position. Replaces
    /// any prior queue. The returned tuple is `(queue, starting_index)` so
    /// callers can also feed the first track to the engine.
    /// Replace the queue wholesale (used by Instant Mix). The caller passes
    /// the index of the track that should start playing.
    pub fn set_play_queue(&mut self, items: Vec<Item>, start_index: usize) {
        if items.is_empty() {
            self.queue.clear();
            self.queue_index = None;
            return;
        }
        let index = start_index.min(items.len() - 1);
        self.queue = items;
        self.queue_index = Some(index);
    }

    pub fn build_queue_for(&mut self, started: &Item) {
        let Some(level) = self.current_level() else {
            self.queue.clear();
            self.queue_index = None;
            return;
        };
        let mut queue: Vec<Item> = Vec::new();
        let mut start_index = 0;
        for item in &level.items {
            if !matches!(MediaKind::classify(item.kind.as_deref()), MediaKind::Audio) {
                continue;
            }
            if item.id == started.id {
                start_index = queue.len();
            }
            queue.push(item.clone());
        }
        if queue.is_empty() {
            queue.push(started.clone());
            start_index = 0;
        }
        self.queue = queue;
        self.queue_index = Some(start_index);
    }

    /// Insert `item` right after the currently-playing track (Play next) or
    /// at the queue tail (Add to queue). No-op when the queue is empty (the
    /// caller already issues a normal Play in that case).
    pub fn enqueue(&mut self, item: Item, play_next: bool) {
        if self.queue.is_empty() {
            self.queue.push(item);
            self.queue_index = Some(0);
            return;
        }
        if play_next {
            let after = self.queue_index.map(|i| i + 1).unwrap_or(self.queue.len());
            let pos = after.min(self.queue.len());
            self.queue.insert(pos, item);
        } else {
            self.queue.push(item);
        }
    }

    /// Advance to the next track in the queue, honoring [`RepeatMode`].
    /// `None` when no further tracks remain (queue ends with repeat off).
    pub fn advance_queue(&mut self) -> Option<Item> {
        let current = self.queue_index?;
        if self.queue.is_empty() {
            return None;
        }
        let next = match self.repeat_mode {
            RepeatMode::One => current,
            RepeatMode::All => (current + 1) % self.queue.len(),
            RepeatMode::Off => {
                let candidate = current + 1;
                if candidate >= self.queue.len() {
                    return None;
                }
                candidate
            }
        };
        self.queue_index = Some(next);
        self.queue.get(next).cloned()
    }

    /// Step back to the previous track. Returns `None` when already at the
    /// start (or repeat is one, which has no notion of "previous").
    pub fn previous_in_queue(&mut self) -> Option<Item> {
        let current = self.queue_index?;
        if self.queue.is_empty() {
            return None;
        }
        let prev = match self.repeat_mode {
            RepeatMode::One => current,
            RepeatMode::All => {
                if current == 0 {
                    self.queue.len() - 1
                } else {
                    current - 1
                }
            }
            RepeatMode::Off => {
                if current == 0 {
                    return None;
                }
                current - 1
            }
        };
        self.queue_index = Some(prev);
        self.queue.get(prev).cloned()
    }

    /// Toggle shuffle. Turning shuffle ON reorders the queue with the current
    /// track pinned at index 0 so playback continues without jumping. Emits a
    /// `SaveAudioPrefs` intent so the new state survives a restart.
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        self.status_message = Some(if self.shuffle {
            "Shuffle on".to_string()
        } else {
            "Shuffle off".to_string()
        });
        if self.shuffle && !self.queue.is_empty() {
            let current_index = self.queue_index.unwrap_or(0);
            let current = self.queue.remove(current_index);
            shuffle_in_place(&mut self.queue);
            self.queue.insert(0, current);
            self.queue_index = Some(0);
        }
        self.queue_save_intent();
    }

    /// Cycle the repeat mode (Off → All → One → Off), flash the new mode in
    /// the status bar, and persist it.
    pub fn cycle_repeat(&mut self) {
        self.repeat_mode = self.repeat_mode.cycle();
        self.status_message = Some(format!("Repeat: {}", self.repeat_mode.label()));
        self.queue_save_intent();
    }

    /// Apply a sleep-timer choice picked from the overlay.
    pub fn pick_sleep_timer(&mut self, choice: SleepTimer) {
        self.sleep_timer = choice;
        self.sleep_remaining_secs = None;
        self.status_message = Some(match choice {
            SleepTimer::Off => "Sleep timer off".to_string(),
            SleepTimer::EndOfTrack => "Sleep timer: stop at end of track".to_string(),
            other => format!("Sleep timer: {}", other.label()),
        });
        self.pending.push(Intent::SetSleepTimer(choice));
    }

    /// Open the sleep-timer overlay with the cursor on the active preset.
    fn open_sleep_timer_picker(&mut self) {
        let idx = SleepTimer::PRESETS
            .iter()
            .position(|p| *p == self.sleep_timer)
            .unwrap_or(0);
        self.popup = Some(PopupMenu::SleepTimer(idx));
    }

    fn queue_save_intent(&mut self) {
        self.pending.push(Intent::SaveAudioPrefs {
            repeat_mode: self.repeat_mode,
            shuffle: self.shuffle,
        });
    }

    /// Clear the queue (used when audio stops without advancing).
    pub fn clear_queue(&mut self) {
        self.queue.clear();
        self.queue_index = None;
    }

    /// Slice of upcoming tracks (the ones after the current). Used by the
    /// queue pane's renderer.
    pub fn upcoming_queue(&self) -> &[Item] {
        match self.queue_index {
            Some(idx) if idx + 1 < self.queue.len() => &self.queue[idx + 1..],
            _ => &[],
        }
    }

    /// The currently-playing queued track, if any.
    pub fn current_queue_track(&self) -> Option<&Item> {
        self.queue.get(self.queue_index?)
    }

    /// Fill the loading level whose `parent_id` matches `id` with fetched items.
    /// Ignored if the user has already navigated away from it.
    pub fn fill_level(&mut self, id: &str, items: Vec<Item>) {
        if let Some(level) = self
            .stack
            .iter_mut()
            .find(|level| level.loading && level.parent_id == id)
        {
            level.items = items;
            level.selected = 0;
            level.loading = false;
        }
    }

    /// Mark the root level loading and queue a section-filtered refetch.
    pub fn apply_section(&mut self, index: usize) {
        let Some(library) = self.current_library().cloned() else {
            return;
        };
        let sections = sections_for(library.collection_type.as_deref());
        let Some(section) = sections.get(index).cloned() else {
            return;
        };
        self.section_selected = index;
        // Remember the user's pick so a 1-9 round-trip lands on the same
        // section next time.
        self.section_memory
            .insert(library.id.clone(), section.name.clone());
        // Reset to a single loading root level matching this library + section.
        self.stack = vec![Level {
            title: format!("{} · {}", library.name, section.name),
            parent_id: library.id.clone(),
            items: Vec::new(),
            selected: 0,
            loading: true,
            parent_item: None,
            cursor_engaged: true,
        }];
        // Fetch first (the primary action), then queue the save so the choice
        // survives restart.
        self.pending.push(Intent::ApplySection {
            library_id: library.id.clone(),
            section,
            extras: self
                .section_filters
                .get(&library.id)
                .cloned()
                .unwrap_or_default(),
        });
        self.pending
            .push(Intent::SaveSectionMemory(self.section_memory.clone()));
    }

    /// Replace the active library's root level items with `items` (used by
    /// `Browser` after both ApplySection and Search fetches complete).
    pub fn apply_root_items(&mut self, library_id: &str, title: String, items: Vec<Item>) {
        if let Some(root) = self.stack.first_mut() {
            if root.parent_id == library_id {
                root.title = title;
                root.items = items;
                root.selected = 0;
                root.loading = false;
                self.stack.truncate(1);
            }
        }
    }

    /// Drop a loading level (e.g. its fetch failed), if it's still on top.
    pub fn drop_loading_level(&mut self, id: &str) {
        if self.is_drilled() {
            if let Some(level) = self.stack.last() {
                if level.loading && level.parent_id == id {
                    self.stack.pop();
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }

        // The error modal captures input until dismissed.
        if self.error.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => {
                    self.error = None;
                    self.error_copied = false;
                }
                KeyCode::Char('y') => {
                    self.error_copied = crate::paths::state_dir()
                        .map(|dir| error_modal::copy_to_clipboard(&dir.display().to_string()))
                        .unwrap_or(false);
                }
                _ => {}
            }
            return;
        }

        // The go-to prefix captures the very next key. Esc cancels.
        if self.awaiting_go_to {
            self.resolve_go_to(key);
            return;
        }

        // Go-to trigger fires from anywhere except the search input (where it
        // would be eaten as a literal character). Overlays it competes with
        // are closed when `resolve_go_to` applies a target.
        if self.search_query.is_none() {
            if let Some(Action::GoTo) = self.keymap.action_for(key) {
                self.awaiting_go_to = true;
                return;
            }
        }

        // Pickers (visibility / playlist / video tracks) sit above the popup
        // menu and capture input until closed.
        if self.library_picker.is_some() {
            self.handle_library_picker_key(key);
            return;
        }
        if self.playlist_picker.is_some() {
            self.handle_playlist_picker_key(key);
            return;
        }
        if self.video_track_picker.is_some() {
            self.handle_video_track_picker_key(key);
            return;
        }
        // Media-options view captures input while the Content pane is focused.
        if self.media_options_view.is_some() && self.focus == Pane::Content {
            self.handle_media_options_key(key);
            return;
        }

        // Any open popup menu captures input until closed.
        if self.popup.is_some() {
            self.handle_popup_key(key);
            return;
        }

        // Global search owns input while focused (own query buffer + result
        // list). Esc leaves; Enter submits or activates a result.
        if self.focus == Pane::GlobalSearch {
            self.handle_global_search_key(key);
            return;
        }

        // The theme picker captures input while open.
        if let Some(selected) = self.theme_picker {
            match key.code {
                KeyCode::Up => self.theme_picker = Some(selected.saturating_sub(1)),
                KeyCode::Down => {
                    let max = self.available_themes.len().saturating_sub(1);
                    self.theme_picker = Some((selected + 1).min(max));
                }
                KeyCode::Enter => {
                    if let Some(name) = self.available_themes.get(selected) {
                        self.pending.push(Intent::SetTheme(name.clone()));
                    }
                    self.theme_picker = None;
                }
                KeyCode::Esc => self.theme_picker = None,
                _ => {}
            }
            return;
        }

        // Any key dismisses the help overlay.
        if self.show_help {
            self.show_help = false;
            return;
        }

        // Search input mode owns every key until Esc/Enter.
        if self.search_query.is_some() {
            self.handle_search_key(key);
            return;
        }

        // Open the search input. '/' is the conventional opener; the search
        // field lives on the top bar.
        if matches!(key.code, KeyCode::Char('/')) && key.modifiers.is_empty() {
            self.search_query = Some(String::new());
            self.focus = Pane::TopBar;
            self.status_message = None;
            return;
        }

        // Top-bar library switch: digit 1..9 picks library N (no modifiers).
        // From Home, the digit also drops focus into the chosen library.
        if let KeyCode::Char(c) = key.code {
            if key.modifiers.is_empty() && c.is_ascii_digit() && c != '0' {
                let index = (c as u8 - b'1') as usize;
                if index < self.libraries.len() {
                    self.status_message = None;
                    let came_from_home = self.focus == Pane::Home;
                    self.select_library(index);
                    if came_from_home {
                        self.focus = Pane::LibraryItems;
                    }
                }
                return;
            }
        }

        let Some(action) = self.keymap.action_for(key) else {
            return;
        };

        // A new keypress clears any transient status note from the last one.
        self.status_message = None;

        // Home dashboard owns navigation while focused. The non-nav actions
        // (play/pause, volume, themes, …) still flow through the normal path
        // so global hotkeys keep working from Home.
        if self.focus == Pane::Home {
            match action {
                Action::Up => { self.move_home_cursor(-1, 0); return; }
                Action::Down => { self.move_home_cursor(1, 0); return; }
                Action::Left => { self.move_home_cursor(0, -1); return; }
                Action::Right => { self.move_home_cursor(0, 1); return; }
                Action::Top => { self.home_top(); return; }
                Action::Bottom => { self.home_bottom(); return; }
                Action::Play => { self.activate_home(); return; }
                Action::Back => { self.focus = Pane::TopBar; return; }
                _ => {}
            }
        }

        match action {
            Action::Quit => self.should_quit = true,
            Action::Up => self.cursor_up(),
            Action::Down => self.cursor_down(),
            Action::Left => self.go_back(),
            Action::Right => self.activate(),
            Action::FocusNext => self.focus_next(),
            Action::FocusPrev => self.focus_prev(),
            Action::Top => self.go_top(),
            Action::Bottom => self.go_bottom(),
            Action::Play => self.activate(),
            Action::Back => self.go_back(),
            Action::PlayPause => self.pending.push(Intent::TogglePause),
            Action::Stop => self.pending.push(Intent::Stop),
            Action::VolumeUp => self.pending.push(Intent::VolumeUp),
            Action::VolumeDown => self.pending.push(Intent::VolumeDown),
            Action::SeekForward => self.pending.push(Intent::SeekForward),
            Action::SeekBackward => self.pending.push(Intent::SeekBackward),
            Action::QueueNext => self.pending.push(Intent::QueueNext),
            Action::QueuePrev => self.pending.push(Intent::QueuePrev),
            Action::QueueShuffle => self.toggle_shuffle(),
            Action::QueueRepeat => self.cycle_repeat(),
            Action::Favorite => self.toggle_favorite(),
            Action::Themes => self.open_theme_picker(),
            Action::ItemMenu => self.open_item_menu(),
            Action::ClientMenu => self.open_client_menu(),
            Action::InfoScrollUp => self.scroll_focused_context(-3),
            Action::InfoScrollDown => self.scroll_focused_context(3),
            Action::Help => self.show_help = true,
            Action::GoTo => self.awaiting_go_to = true,
            Action::Cancel => {}
        }
    }

    /// Resolve the second keystroke of the go-to chord into a focus / view
    /// change. Esc cancels the chord; any unhandled key just clears the flag.
    fn resolve_go_to(&mut self, key: KeyEvent) {
        self.awaiting_go_to = false;
        // Esc cancels without dismissing overlays.
        if matches!(key.code, KeyCode::Esc) {
            return;
        }
        // Any concrete go-to target dismisses overlays that might cover the
        // destination pane (popup menus, pickers, theme picker, the
        // media-options view, the help overlay).
        if matches!(
            key.code,
            KeyCode::Char('t' | 'i' | 's' | 'c' | 'n' | 'l' | 'q' | 'h')
        ) {
            self.popup = None;
            self.library_picker = None;
            self.playlist_picker = None;
            self.video_track_picker = None;
            self.media_options_view = None;
            self.theme_picker = None;
            self.show_help = false;
        }
        match key.code {
            KeyCode::Char('t') => self.focus = Pane::TopBar,
            KeyCode::Char('i') => self.focus = Pane::LibraryItems,
            KeyCode::Char('s') => self.enter_global_search(),
            KeyCode::Char('c') => self.focus = Pane::Content,
            KeyCode::Char('n') => {
                self.focus = Pane::ContextTop;
                self.set_context_view(ContextTopView::Info);
            }
            KeyCode::Char('l') => {
                self.focus = Pane::ContextTop;
                self.set_context_view(ContextTopView::Lyrics);
            }
            KeyCode::Char('q') => self.focus = Pane::ContextBottom,
            KeyCode::Char('h') => self.enter_home(),
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
    }

    /// Accessor for the renderer: `true` while the go-to cheatsheet should be
    /// shown in the status row.
    pub fn awaiting_go_to(&self) -> bool {
        self.awaiting_go_to
    }

    /// Handle a keystroke while the search input is focused.
    fn handle_search_key(&mut self, key: KeyEvent) {
        let Some(query) = self.search_query.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.search_query = None;
                self.search_history_cursor = None;
                self.focus = Pane::LibraryItems;
            }
            KeyCode::Enter => {
                let q = query.trim().to_string();
                self.search_query = None;
                self.search_history_cursor = None;
                self.focus = Pane::LibraryItems;
                if !q.is_empty() {
                    self.remember_search(&q);
                    self.start_search(q);
                }
            }
            KeyCode::Backspace => {
                query.pop();
                self.search_history_cursor = None;
            }
            // Up/Down walk the recent-search list. Newest is index 0.
            KeyCode::Up => self.cycle_search_history(1),
            KeyCode::Down => self.cycle_search_history(-1),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                query.push(c);
                self.search_history_cursor = None;
            }
            _ => {}
        }
    }

    /// Move through `search_history` in the direction `delta` (+1 = older,
    /// -1 = newer) and rewrite the search input to match.
    fn cycle_search_history(&mut self, delta: i32) {
        if self.search_history.is_empty() {
            return;
        }
        let max = self.search_history.len() as i32 - 1;
        let next: i32 = match self.search_history_cursor {
            Some(c) => (c as i32 + delta).clamp(-1, max),
            None if delta > 0 => 0,
            None => return,
        };
        if next < 0 {
            self.search_history_cursor = None;
            self.search_query = Some(String::new());
        } else {
            let i = next as usize;
            self.search_history_cursor = Some(i);
            self.search_query = Some(self.search_history[i].clone());
        }
    }

    /// Push `query` to the front of `search_history`, dedup, cap at 20, and
    /// queue a save so the new list survives restart.
    fn remember_search(&mut self, query: &str) {
        const MAX: usize = 20;
        self.search_history.retain(|q| q != query);
        self.search_history.insert(0, query.to_string());
        if self.search_history.len() > MAX {
            self.search_history.truncate(MAX);
        }
        self.pending
            .push(Intent::SaveSearchHistory(self.search_history.clone()));
    }

    /// Push a loading root level and queue the search fetch. The active
    /// section narrows the search to its item types — section "All" (empty
    /// `item_types`) searches every type in the library.
    fn start_search(&mut self, query: String) {
        let Some(library) = self.current_library().cloned() else {
            return;
        };
        let section = self.current_section();
        let item_types = section
            .as_ref()
            .map(|s| s.item_types.clone())
            .unwrap_or_default();
        let scope_label = match section.as_ref() {
            Some(s) if !s.item_types.is_empty() => s.name.clone(),
            _ => String::new(),
        };
        self.stack = vec![Level {
            title: search_level_title(&scope_label, &query),
            parent_id: library.id.clone(),
            items: Vec::new(),
            selected: 0,
            loading: true,
            parent_item: None,
            cursor_engaged: true,
        }];
        self.pending.push(Intent::Search {
            query,
            item_types,
            scope_label,
        });
    }

    /// Popup state for the renderer: which menu is open and which entry is highlighted.
    pub fn popup_menu(&self) -> Option<(&'static str, Vec<String>, usize)> {
        match self.popup? {
            PopupMenu::Item(selected) => {
                let entries = self
                    .item_menu_entries()
                    .into_iter()
                    .map(|(label, _)| label)
                    .collect();
                Some(("Actions", entries, selected))
            }
            PopupMenu::Client(selected) => {
                let entries = self
                    .client_menu_entries()
                    .into_iter()
                    .map(|(label, _)| label)
                    .collect();
                Some(("Client settings", entries, selected))
            }
            PopupMenu::SleepTimer(selected) => {
                let entries = self
                    .sleep_timer_entries()
                    .into_iter()
                    .map(|(label, _)| label)
                    .collect();
                Some(("Sleep timer", entries, selected))
            }
        }
    }

    fn sleep_timer_entries(&self) -> Vec<(String, SleepTimer)> {
        SleepTimer::PRESETS
            .iter()
            .map(|preset| {
                let mut label = match preset {
                    SleepTimer::Off => {
                        if self.sleep_timer == SleepTimer::Off {
                            "Off".to_string()
                        } else {
                            "Cancel timer".to_string()
                        }
                    }
                    other => other.label().to_string(),
                };
                if *preset == self.sleep_timer {
                    if let Some(secs) = self.sleep_remaining_secs {
                        label.push_str(&format!("  · {} left", format_remaining(secs)));
                    } else if *preset != SleepTimer::Off {
                        label.push_str("  · active");
                    }
                }
                (label, *preset)
            })
            .collect()
    }

    /// Entries shown in the per-item popup. Built from the focused item so the
    /// list reflects what's actually possible (e.g. Play is hidden on folders).
    fn item_menu_entries(&self) -> Vec<(String, ItemMenuAction)> {
        let mut entries: Vec<(String, ItemMenuAction)> = Vec::new();
        let Some(item) = self.current_item() else {
            return entries;
        };
        let is_audio = matches!(MediaKind::classify(item.kind.as_deref()), MediaKind::Audio);
        // Both view entries are present for audio so the user can always pick
        // either lyrics or info; each entry switches the context pane to its
        // view and fetches the data it needs.
        if is_audio {
            entries.push(("Show lyrics".to_string(), ItemMenuAction::ShowLyrics));
            entries.push(("Show info".to_string(), ItemMenuAction::LoadInfo));
        } else {
            entries.push(("Load info".to_string(), ItemMenuAction::LoadInfo));
        }
        let playable = !item.is_folder
            && matches!(
                MediaKind::classify(item.kind.as_deref()),
                MediaKind::Video | MediaKind::Audio,
            );
        if playable {
            entries.push(("Play".to_string(), ItemMenuAction::Play));
        }
        let fav_label = if item.is_favorite {
            "Remove from favorites"
        } else {
            "Add to favorites"
        };
        entries.push((fav_label.to_string(), ItemMenuAction::ToggleFavorite));
        if playable {
            let played_label = if item.is_played {
                "Mark unplayed"
            } else {
                "Mark played"
            };
            entries.push((played_label.to_string(), ItemMenuAction::TogglePlayed));
        }
        if let Some(detail) = self.current_detail() {
            if let Some(first_genre) = detail.genres.first() {
                entries.push((
                    format!("Browse by genre: {first_genre}"),
                    ItemMenuAction::BrowseByGenre,
                ));
            }
            if let Some(actor) = detail
                .cast
                .iter()
                .find(|p| p.id.is_some() && !p.name.is_empty())
            {
                entries.push((
                    format!("Browse by person: {}", actor.name),
                    ItemMenuAction::BrowseByPerson,
                ));
            }
        }
        if let Some(label) = go_to_artist_label(item) {
            entries.push((label, ItemMenuAction::GoToArtist));
        }
        if let Some(label) = go_to_album_label(item) {
            entries.push((label, ItemMenuAction::GoToAlbum));
        }
        entries.push(("Add to playlist…".to_string(), ItemMenuAction::AddToPlaylist));
        if is_audio {
            entries.push(("Play next".to_string(), ItemMenuAction::PlayNext));
            entries.push(("Add to queue".to_string(), ItemMenuAction::AddToQueue));
            entries.push(("Instant mix".to_string(), ItemMenuAction::InstantMix));
            entries.push((
                "Genre radio".to_string(),
                ItemMenuAction::GenreRadio,
            ));
            entries.push((
                "Save queue as playlist…".to_string(),
                ItemMenuAction::SaveQueueAsPlaylist,
            ));
            entries.push(("Dislike track".to_string(), ItemMenuAction::Dislike));
        }
        if self
            .current_detail()
            .is_some_and(|d| !d.trailer_urls.is_empty())
        {
            entries.push(("Watch trailer".to_string(), ItemMenuAction::WatchTrailer));
        }
        let is_video = matches!(
            MediaKind::classify(item.kind.as_deref()),
            MediaKind::Video,
        );
        if is_video {
            let sel = self
                .video_track_selections
                .get(item.id.as_str())
                .cloned()
                .unwrap_or_default();
            entries.push((
                format!("Audio track: {}", track_choice_label(sel.audio)),
                ItemMenuAction::AudioTrack,
            ));
            entries.push((
                format!("Subtitles: {}", track_choice_label(sel.subtitle)),
                ItemMenuAction::Subtitles,
            ));
        }
        entries.push((
            format!("Shuffle: {}", if self.shuffle { "on" } else { "off" }),
            ItemMenuAction::ToggleShuffle,
        ));
        entries.push((
            format!("Repeat: {}", self.repeat_mode.label()),
            ItemMenuAction::CycleRepeat,
        ));
        entries.push((
            sleep_timer_menu_label(self.sleep_timer, self.sleep_remaining_secs),
            ItemMenuAction::OpenSleepTimerPicker,
        ));
        entries.push((
            "Copy URL to clipboard".to_string(),
            ItemMenuAction::CopyUrl,
        ));
        entries
    }

    /// Entries shown in the client-settings popup. Labels carry the current
    /// state so the user can see what they'd be toggling.
    fn client_menu_entries(&self) -> Vec<(String, ClientMenuAction)> {
        let current_extras = self
            .current_library()
            .and_then(|l| self.section_filters.get(&l.id).cloned())
            .unwrap_or_default();
        let sort_label = current_extras
            .sort_override
            .as_ref()
            .and_then(|s| s.first().cloned())
            .unwrap_or_else(|| "Section default".to_string());
        let unplayed_on = current_extras.filters.iter().any(|f| f == "IsUnplayed");
        let any_filter = !current_extras.genres.is_empty()
            || !current_extras.person_ids.is_empty()
            || !current_extras.studio_ids.is_empty()
            || !current_extras.years.is_empty()
            || !current_extras.tags.is_empty()
            || !current_extras.filters.is_empty()
            || current_extras.sort_override.is_some();
        vec![
            ("Theme…".to_string(), ClientMenuAction::Themes),
            ("Sync with server now".to_string(), ClientMenuAction::SyncNow),
            (
                "Visible libraries…".to_string(),
                ClientMenuAction::VisibleLibraries,
            ),
            (
                format!("Sort by: {sort_label}"),
                ClientMenuAction::CycleSort,
            ),
            (
                format!("Show only unplayed: {}", on_off(unplayed_on)),
                ClientMenuAction::ToggleUnplayed,
            ),
            (
                format!(
                    "Clear filters ({})",
                    if any_filter { "active" } else { "none" }
                ),
                ClientMenuAction::ClearFilters,
            ),
            (
                format!("Gapless audio: {}", on_off(self.gapless)),
                ClientMenuAction::ToggleGapless,
            ),
            (
                format!("Volume normalization: {}", on_off(self.normalization)),
                ClientMenuAction::ToggleNormalization,
            ),
            (
                format!("EQ: {}", eq_preset_label(self.eq_preset)),
                ClientMenuAction::CycleEqPreset,
            ),
            (
                format!("Custom mpv args: {}", self.mpv_arg_count),
                ClientMenuAction::ShowMpvArgs,
            ),
            ("Quit".to_string(), ClientMenuAction::Quit),
        ]
    }

    fn open_item_menu(&mut self) {
        if self.current_item().is_none() {
            return;
        }
        self.popup = Some(PopupMenu::Item(0));
    }

    fn open_client_menu(&mut self) {
        self.popup = Some(PopupMenu::Client(0));
    }

    /// Route key input while a popup menu is open.
    fn handle_popup_key(&mut self, key: KeyEvent) {
        let Some(popup) = self.popup else { return };
        let len = match popup {
            PopupMenu::Item(_) => self.item_menu_entries().len(),
            PopupMenu::Client(_) => self.client_menu_entries().len(),
            PopupMenu::SleepTimer(_) => self.sleep_timer_entries().len(),
        };
        if len == 0 {
            self.popup = None;
            return;
        }
        let selected = match popup {
            PopupMenu::Item(i) | PopupMenu::Client(i) | PopupMenu::SleepTimer(i) => i.min(len - 1),
        };
        match key.code {
            KeyCode::Esc => self.popup = None,
            KeyCode::Up => {
                let next = selected.saturating_sub(1);
                self.popup = Some(match popup {
                    PopupMenu::Item(_) => PopupMenu::Item(next),
                    PopupMenu::Client(_) => PopupMenu::Client(next),
                    PopupMenu::SleepTimer(_) => PopupMenu::SleepTimer(next),
                });
            }
            KeyCode::Down => {
                let next = (selected + 1).min(len - 1);
                self.popup = Some(match popup {
                    PopupMenu::Item(_) => PopupMenu::Item(next),
                    PopupMenu::Client(_) => PopupMenu::Client(next),
                    PopupMenu::SleepTimer(_) => PopupMenu::SleepTimer(next),
                });
            }
            KeyCode::Enter => {
                self.popup = None;
                match popup {
                    PopupMenu::Item(_) => {
                        if let Some((_, action)) =
                            self.item_menu_entries().into_iter().nth(selected)
                        {
                            self.activate_item_action(action);
                        }
                    }
                    PopupMenu::Client(_) => {
                        if let Some((_, action)) =
                            self.client_menu_entries().into_iter().nth(selected)
                        {
                            self.activate_client_action(action);
                        }
                    }
                    PopupMenu::SleepTimer(_) => {
                        if let Some((_, choice)) =
                            self.sleep_timer_entries().into_iter().nth(selected)
                        {
                            self.pick_sleep_timer(choice);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn activate_item_action(&mut self, action: ItemMenuAction) {
        match action {
            ItemMenuAction::LoadInfo => {
                let Some(item) = self.current_item().cloned() else {
                    return;
                };
                self.set_context_view(ContextTopView::Info);
                self.pending.push(Intent::LoadCurrentDetail {
                    item_id: item.id,
                    kind: item.kind,
                });
            }
            ItemMenuAction::ShowLyrics => {
                self.set_context_view(ContextTopView::Lyrics);
                // Lyrics still need the lyric body to be fetched; reuse the
                // detail intent so the loop drives the request.
                if let Some(item) = self.current_item().cloned() {
                    self.pending.push(Intent::LoadCurrentDetail {
                        item_id: item.id,
                        kind: item.kind,
                    });
                }
            }
            ItemMenuAction::Play => self.activate(),
            ItemMenuAction::ToggleFavorite => self.toggle_favorite(),
            ItemMenuAction::TogglePlayed => self.toggle_played(),
            ItemMenuAction::BrowseByGenre => self.browse_by_first_genre(),
            ItemMenuAction::BrowseByPerson => self.browse_by_first_person(),
            ItemMenuAction::GoToAlbum => self.go_to_album(),
            ItemMenuAction::GoToArtist => self.go_to_artist(),
            ItemMenuAction::PlayNext => self.play_next_or_append(true),
            ItemMenuAction::AddToQueue => self.play_next_or_append(false),
            ItemMenuAction::GenreRadio => self.start_genre_radio(),
            ItemMenuAction::SaveQueueAsPlaylist => self.save_queue_as_playlist(),
            ItemMenuAction::WatchTrailer => self.watch_first_trailer(),
            ItemMenuAction::AddToPlaylist => self.open_playlist_picker(),
            ItemMenuAction::InstantMix => {
                if let Some(item) = self.current_item().cloned() {
                    self.set_status(format!("Instant mix: {}", item.name));
                    self.pending.push(Intent::InstantMix { item });
                }
            }
            ItemMenuAction::Dislike => {
                if let Some(item) = self.current_item().cloned() {
                    self.set_status(format!("Disliked: {}", item.name));
                    self.pending.push(Intent::Dislike { item_id: item.id });
                }
            }
            ItemMenuAction::AudioTrack => self.open_video_track_picker(VideoTrackKind::Audio),
            ItemMenuAction::Subtitles => self.open_video_track_picker(VideoTrackKind::Subtitle),
            ItemMenuAction::ToggleShuffle => self.toggle_shuffle(),
            ItemMenuAction::CycleRepeat => self.cycle_repeat(),
            ItemMenuAction::OpenSleepTimerPicker => self.open_sleep_timer_picker(),
            ItemMenuAction::CopyUrl => {
                if let Some(item) = self.current_item().cloned() {
                    self.pending.push(Intent::CopyItemUrl {
                        item_id: item.id,
                        item_name: item.name,
                    });
                }
            }
        }
    }

    fn activate_client_action(&mut self, action: ClientMenuAction) {
        match action {
            ClientMenuAction::Themes => self.open_theme_picker(),
            ClientMenuAction::SyncNow => {
                self.set_status("Syncing with server…");
                self.pending.push(Intent::SyncLibraries);
            }
            ClientMenuAction::VisibleLibraries => self.open_library_picker(),
            ClientMenuAction::ToggleGapless => {
                self.gapless = !self.gapless;
                self.status_message = Some(format!(
                    "Gapless audio: {}",
                    on_off(self.gapless)
                ));
                self.pending.push(Intent::SetGapless(self.gapless));
            }
            ClientMenuAction::ToggleNormalization => {
                self.normalization = !self.normalization;
                self.status_message = Some(format!(
                    "Volume normalization: {}",
                    on_off(self.normalization)
                ));
                self.pending
                    .push(Intent::SetNormalization(self.normalization));
            }
            ClientMenuAction::CycleEqPreset => {
                self.eq_preset = cycle_eq_preset(self.eq_preset);
                self.status_message = Some(format!("EQ: {}", eq_preset_label(self.eq_preset)));
                self.pending.push(Intent::SetEqPreset(self.eq_preset));
            }
            ClientMenuAction::ShowMpvArgs => {
                self.status_message = Some(format!(
                    "Custom mpv args: {} (edit [video] mpv_args in config.toml)",
                    self.mpv_arg_count
                ));
            }
            ClientMenuAction::CycleSort => self.cycle_section_sort(),
            ClientMenuAction::ToggleUnplayed => self.toggle_section_unplayed(),
            ClientMenuAction::ClearFilters => self.clear_section_filters(),
            ClientMenuAction::Quit => self.should_quit = true,
        }
    }

    fn cycle_section_sort(&mut self) {
        let Some(library_id) = self.current_library().map(|l| l.id.clone()) else {
            return;
        };
        let extras = self.section_filters.entry(library_id).or_default();
        extras.sort_override = next_sort_override(extras.sort_override.as_deref());
        let label = extras
            .sort_override
            .as_ref()
            .and_then(|s| s.first().cloned())
            .unwrap_or_else(|| "Section default".to_string());
        self.status_message = Some(format!("Sort by: {label}"));
        self.reapply_current_section();
    }

    fn toggle_section_unplayed(&mut self) {
        let Some(library_id) = self.current_library().map(|l| l.id.clone()) else {
            return;
        };
        let extras = self.section_filters.entry(library_id).or_default();
        if let Some(pos) = extras.filters.iter().position(|f| f == "IsUnplayed") {
            extras.filters.remove(pos);
            self.status_message = Some("Showing all".to_string());
        } else {
            extras.filters.push("IsUnplayed".to_string());
            self.status_message = Some("Showing only unplayed".to_string());
        }
        self.reapply_current_section();
    }

    fn clear_section_filters(&mut self) {
        let Some(library_id) = self.current_library().map(|l| l.id.clone()) else {
            return;
        };
        self.section_filters.remove(&library_id);
        self.status_message = Some("Filters cleared".to_string());
        self.reapply_current_section();
    }

    /// Fire the active library's current section again, picking up whatever
    /// runtime extras live in `section_filters[library_id]`.
    fn reapply_current_section(&mut self) {
        let idx = self.section_selected;
        self.apply_section(idx);
    }

    /// Insert the focused audio item into the queue (play-next or append).
    fn play_next_or_append(&mut self, play_next: bool) {
        let Some(item) = self.current_item().cloned() else { return };
        if !matches!(MediaKind::classify(item.kind.as_deref()), MediaKind::Audio) {
            self.set_status("Only audio items can be queued.");
            return;
        }
        if self.queue.is_empty() {
            self.pending.push(Intent::Play {
                item,
                media: MediaKind::Audio,
                start_ticks: None,
            });
            return;
        }
        self.status_message = Some(if play_next {
            format!("Play next: {}", item.name)
        } else {
            format!("Added to queue: {}", item.name)
        });
        self.enqueue(item, play_next);
    }

    /// Start a "genre radio" — queue items with the first genre from the
    /// current detail and play. Delegates to the browser via a new intent.
    fn start_genre_radio(&mut self) {
        let Some(library) = self.current_library().cloned() else { return };
        let Some(genre) = self
            .current_detail()
            .and_then(|d| d.genres.first().cloned())
        else {
            self.set_status("No genre on this item.");
            return;
        };
        self.status_message = Some(format!("Genre radio: {genre}"));
        self.pending.push(Intent::GenreRadio {
            library_id: library.id,
            genre,
        });
    }

    /// Snapshot the queue and ask the browser to create a server-side playlist.
    fn save_queue_as_playlist(&mut self) {
        if self.queue.is_empty() {
            self.set_status("Queue is empty.");
            return;
        }
        let name = format!(
            "aquafin queue · {}",
            chrono_like_now_label()
        );
        let item_ids: Vec<String> = self.queue.iter().map(|i| i.id.clone()).collect();
        self.status_message = Some(format!("Saving queue as \"{name}\"…"));
        self.pending.push(Intent::CreatePlaylist { name, item_ids });
    }

    /// Launch mpv on the first trailer URL from the current item's detail.
    fn watch_first_trailer(&mut self) {
        let Some(item) = self.current_item().cloned() else { return };
        let url = self
            .current_detail()
            .and_then(|d| d.trailer_urls.first().cloned());
        match url {
            Some(url) => {
                self.set_status(format!("Trailer: {}", item.name));
                self.pending.push(Intent::WatchTrailer { url, title: item.name });
            }
            None => self.set_status("No trailer available for this item."),
        }
    }

    /// Use the first genre from `current_detail` as a filter on the active
    /// library, then re-apply the "All" section.
    /// Drill into the focused item's parent album (track → album view).
    fn go_to_album(&mut self) {
        let Some(item) = self.current_item().cloned() else {
            return;
        };
        let (Some(album_id), Some(album_name)) = (
            item.album_id.clone(),
            item.album_name.clone().or_else(|| Some("Album".to_string())),
        ) else {
            self.set_status("No album on this item.");
            return;
        };
        let album = Item {
            id: album_id,
            name: album_name,
            kind: Some("MusicAlbum".to_string()),
            is_folder: true,
            ..Item::default()
        };
        self.drill_into(&album);
    }

    /// Drill into the focused item's primary artist (track/album → artist view).
    fn go_to_artist(&mut self) {
        let Some(item) = self.current_item().cloned() else {
            return;
        };
        let Some(artist_id) = item.primary_artist_id.clone() else {
            self.set_status("No artist on this item.");
            return;
        };
        let artist_name = item
            .primary_artist_name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Artist".to_string());
        let artist = Item {
            id: artist_id,
            name: artist_name,
            kind: Some("MusicArtist".to_string()),
            is_folder: true,
            ..Item::default()
        };
        self.drill_into(&artist);
    }

    fn browse_by_first_genre(&mut self) {
        let Some(library) = self.current_library().cloned() else {
            return;
        };
        let Some(genre) = self
            .current_detail()
            .and_then(|d| d.genres.first().cloned())
        else {
            self.set_status("No genre on this item.");
            return;
        };
        let extras = self.section_filters.entry(library.id.clone()).or_default();
        extras.genres = vec![genre.clone()];
        self.section_selected = 0;
        self.status_message = Some(format!("Browsing by genre: {genre}"));
        self.reapply_current_section();
    }

    /// Use the first cast member id from `current_detail` as a filter.
    fn browse_by_first_person(&mut self) {
        let Some(library) = self.current_library().cloned() else {
            return;
        };
        let Some((person_id, person_name)) =
            self.current_detail().and_then(|d| {
                d.cast
                    .iter()
                    .find_map(|p| p.id.clone().map(|id| (id, p.name.clone())))
            })
        else {
            self.set_status("No person on this item.");
            return;
        };
        let extras = self.section_filters.entry(library.id.clone()).or_default();
        extras.person_ids = vec![person_id];
        self.section_selected = 0;
        self.status_message = Some(format!("Browsing by person: {person_name}"));
        self.reapply_current_section();
    }

    /// Open the visible-library picker. Entries are filled async via
    /// `Intent::LoadAllLibraryMeta`.
    fn open_library_picker(&mut self) {
        self.library_picker = Some(LibraryPickerState {
            entries: Vec::new(),
            selected: 0,
            loading: true,
        });
        self.pending.push(Intent::LoadAllLibraryMeta);
    }

    /// Open the audio / subtitle track picker for the focused video item.
    fn open_video_track_picker(&mut self, kind: VideoTrackKind) {
        let Some(item) = self.current_item().cloned() else {
            return;
        };
        if !matches!(MediaKind::classify(item.kind.as_deref()), MediaKind::Video) {
            return;
        }
        self.video_track_picker = Some(VideoTrackPickerState {
            item_id: item.id.clone(),
            item_name: item.name,
            kind,
            entries: None,
            selected: 0,
        });
        self.pending.push(Intent::LoadVideoTracks {
            item_id: item.id,
            kind,
        });
    }

    /// Picker accessor for the renderer.
    pub fn video_track_picker(&self) -> Option<&VideoTrackPickerState> {
        self.video_track_picker.as_ref()
    }

    /// Populate the open video track picker (called by the loop on response).
    pub fn set_video_track_picker_entries(&mut self, entries: Vec<TrackPickerEntry>) {
        if let Some(picker) = self.video_track_picker.as_mut() {
            picker.entries = Some(entries);
            picker.selected = 0;
        }
    }

    /// Read-only access for [`Playback`] when launching mpv.
    pub fn video_options_for(&self, item_id: &str) -> VideoOptions {
        let selection = self
            .video_track_selections
            .get(item_id)
            .cloned()
            .unwrap_or_default();
        VideoOptions {
            audio: Some(selection.audio),
            subtitle: Some(selection.subtitle),
            start_secs: None,
            extra_args: Vec::new(),
        }
    }

    /// The chosen alternate media-source id for `item_id`, when one was
    /// picked in the media-options view. `None` ⇒ use Jellyfin's default.
    pub fn video_media_source_for(&self, item_id: &str) -> Option<String> {
        self.video_track_selections
            .get(item_id)
            .and_then(|s| s.media_source_id.clone())
    }

    /// Open the playlist picker for the focused audio item. Entries are
    /// filled async via `Intent::LoadPlaylists`.
    fn open_playlist_picker(&mut self) {
        let Some(item) = self.current_item().cloned() else {
            return;
        };
        self.playlist_picker = Some(PlaylistPickerState {
            target_item_id: item.id.clone(),
            target_item_name: item.name,
            entries: None,
            selected: 0,
        });
        self.pending.push(Intent::LoadPlaylists {
            target_item_id: item.id,
        });
    }

    /// Picker accessor for the renderer.
    pub fn library_picker(&self) -> Option<&LibraryPickerState> {
        self.library_picker.as_ref()
    }

    /// Picker accessor for the renderer.
    pub fn playlist_picker(&self) -> Option<&PlaylistPickerState> {
        self.playlist_picker.as_ref()
    }

    /// Populate the open library picker with the server's full library list,
    /// marking each entry's current visibility. Called by the loop on
    /// `LoadAllLibraryMeta` completion.
    pub fn set_library_picker_entries(&mut self, entries: Vec<(String, String, bool)>) {
        if let Some(picker) = self.library_picker.as_mut() {
            picker.entries = entries;
            picker.loading = false;
            picker.selected = 0;
        }
    }

    /// Populate the open playlist picker with the user's playlists.
    pub fn set_playlist_picker_entries(&mut self, entries: Vec<(String, String)>) {
        if let Some(picker) = self.playlist_picker.as_mut() {
            picker.entries = Some(entries);
            picker.selected = 0;
        }
    }

    /// Replace `libraries` after a sync. Resets the drill stack to the active
    /// library's root; falls back to library 0 if the previous selection's id
    /// no longer exists.
    pub fn replace_libraries(&mut self, libraries: Vec<Library>) {
        let previous_id = self
            .libraries
            .get(self.library_selected)
            .map(|l| l.id.clone());
        self.libraries = libraries;
        self.library_selected = previous_id
            .and_then(|id| self.libraries.iter().position(|l| l.id == id))
            .unwrap_or(0);
        self.reset_stack_for_library();
        self.set_status("Sync complete");
    }

    fn handle_library_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.library_picker.as_mut() else {
            return;
        };
        if picker.loading {
            if matches!(key.code, KeyCode::Esc) {
                self.library_picker = None;
            }
            return;
        }
        let len = picker.entries.len();
        if len == 0 {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                self.library_picker = None;
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.library_picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(len - 1),
            KeyCode::Char(' ') => {
                let i = picker.selected;
                picker.entries[i].2 = !picker.entries[i].2;
            }
            KeyCode::Enter => {
                let visible: Vec<String> = picker
                    .entries
                    .iter()
                    .filter(|(_, _, on)| *on)
                    .map(|(id, _, _)| id.clone())
                    .collect();
                self.library_picker = None;
                self.set_status("Saving libraries…");
                self.pending.push(Intent::SaveVisibleLibraries(visible));
            }
            _ => {}
        }
    }

    fn handle_video_track_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.video_track_picker.as_mut() else {
            return;
        };
        let Some(entries) = picker.entries.as_ref() else {
            if matches!(key.code, KeyCode::Esc) {
                self.video_track_picker = None;
            }
            return;
        };
        if entries.is_empty() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                self.video_track_picker = None;
            }
            return;
        }
        let len = entries.len();
        match key.code {
            KeyCode::Esc => self.video_track_picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(len - 1),
            KeyCode::Enter => {
                let entry = entries[picker.selected].clone();
                let item_id = picker.item_id.clone();
                let item_name = picker.item_name.clone();
                let kind = picker.kind;
                self.video_track_picker = None;
                let selection = self
                    .video_track_selections
                    .entry(item_id)
                    .or_default();
                match kind {
                    VideoTrackKind::Audio => selection.audio = entry.choice,
                    VideoTrackKind::Subtitle => selection.subtitle = entry.choice,
                }
                let kind_label = match kind {
                    VideoTrackKind::Audio => "Audio",
                    VideoTrackKind::Subtitle => "Subtitles",
                };
                self.set_status(format!("{kind_label} → {} ({})", entry.label, item_name));
            }
            _ => {}
        }
    }

    fn handle_playlist_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.playlist_picker.as_mut() else {
            return;
        };
        let Some(entries) = picker.entries.as_ref() else {
            if matches!(key.code, KeyCode::Esc) {
                self.playlist_picker = None;
            }
            return;
        };
        if entries.is_empty() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                self.playlist_picker = None;
            }
            return;
        }
        let len = entries.len();
        match key.code {
            KeyCode::Esc => self.playlist_picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(len - 1),
            KeyCode::Enter => {
                let (playlist_id, playlist_name) = entries[picker.selected].clone();
                let item_id = picker.target_item_id.clone();
                let item_name = picker.target_item_name.clone();
                self.playlist_picker = None;
                self.set_status(format!("Added “{item_name}” → {playlist_name}"));
                self.pending.push(Intent::AddToPlaylist {
                    playlist_id,
                    item_id,
                });
            }
            _ => {}
        }
    }

    /// Open the theme picker on the currently-active theme.
    fn open_theme_picker(&mut self) {
        if self.available_themes.is_empty() {
            return;
        }
        let current = self
            .available_themes
            .iter()
            .position(|name| name == self.theme.name())
            .unwrap_or(0);
        self.theme_picker = Some(current);
    }

    /// Enter on the focused pane: apply a section, drill into a folder, play a
    /// leaf, or report that the item can't be played.
    fn activate(&mut self) {
        if self.focus == Pane::LibrarySections {
            self.apply_section(self.section_selected);
            return;
        }
        // Enter on the root pane while drilled replaces the drilled stack, so
        // opening a new album/playlist swaps the middle pane to its contents
        // instead of pushing yet another level.
        let item = if self.focus == Pane::LibraryItems {
            if self.is_drilled() {
                self.stack.truncate(1);
            }
            self.stack
                .first()
                .and_then(|l| l.items.get(l.selected))
                .cloned()
        } else if self.focus == Pane::Content && self.is_drilled() {
            // Activating the drilled middle pane must hit the *child* the
            // cursor sits on, not the parent. `current_item()` pins to
            // `parent_item` while `cursor_engaged` is false (so the info pane
            // doesn't flicker), so without engaging here Right/Enter would
            // re-drill into the same parent forever.
            if let Some(level) = self.stack.last_mut() {
                level.cursor_engaged = true;
            }
            self.stack
                .last()
                .and_then(|l| l.items.get(l.selected))
                .cloned()
        } else {
            self.current_item().cloned()
        };
        let Some(item) = item else {
            return;
        };
        if item.is_folder {
            self.drill_into(&item);
            return;
        }
        match MediaKind::classify(item.kind.as_deref()) {
            MediaKind::Video => self.open_media_options_view(item),
            MediaKind::Audio => self.pending.push(Intent::Play {
                item,
                media: MediaKind::Audio,
                start_ticks: None,
            }),
            MediaKind::Other => {
                self.status_message = Some(format!("Not playable: {}", item.name));
            }
        }
    }

    /// Reveal the item (so the info pane fetches) and open the pre-play
    /// content pane with the version + track lists. The lists are populated
    /// async via `Intent::LoadMediaOptions`.
    fn open_media_options_view(&mut self, item: Item) {
        self.reveal_item(item.id.clone());
        self.set_context_view(ContextTopView::Info);
        self.pending.push(Intent::LoadCurrentDetail {
            item_id: item.id.clone(),
            kind: item.kind.clone(),
        });
        self.media_options_view = Some(MediaOptionsViewState {
            item_id: item.id.clone(),
            item_name: item.name.clone(),
            loading: true,
            versions: Vec::new(),
            audio_entries: Vec::new(),
            subtitle_entries: Vec::new(),
            selected_version: 0,
            selected_audio: 0,
            selected_subtitle: 0,
            cursor: MediaOptionsCursor::Play,
            trailer_urls: Vec::new(),
            chapters: Vec::new(),
        });
        self.focus = Pane::Content;
        self.pending.push(Intent::LoadMediaOptions { item_id: item.id });
    }

    /// Renderer accessor.
    pub fn media_options_view(&self) -> Option<&MediaOptionsViewState> {
        self.media_options_view.as_ref()
    }

    /// Fill the media-options view with the freshly-fetched lists. Preserves
    /// any prior per-item selection so reopening the view remembers choices.
    pub fn set_media_options_view_data(
        &mut self,
        item_id: &str,
        versions: Vec<MediaVersion>,
        audio_entries: Vec<TrackPickerEntry>,
        subtitle_entries: Vec<TrackPickerEntry>,
    ) {
        let Some(view) = self.media_options_view.as_mut() else {
            return;
        };
        if view.item_id != item_id {
            return;
        }
        let stored = self.video_track_selections.get(item_id);
        let stored_version = stored.and_then(|s| s.media_source_id.clone());
        view.selected_version = stored_version
            .as_deref()
            .and_then(|sid| versions.iter().position(|v| v.source_id == sid))
            .unwrap_or(0);
        view.selected_audio = stored
            .map(|s| s.audio)
            .and_then(|c| audio_entries.iter().position(|e| e.choice == c))
            .unwrap_or(0);
        view.selected_subtitle = stored
            .map(|s| s.subtitle)
            .and_then(|c| subtitle_entries.iter().position(|e| e.choice == c))
            .unwrap_or(0);
        view.versions = versions;
        view.audio_entries = audio_entries;
        view.subtitle_entries = subtitle_entries;
        view.loading = false;
        view.cursor = if view.versions.len() > 1 {
            MediaOptionsCursor::Version(view.selected_version)
        } else if !view.audio_entries.is_empty() {
            MediaOptionsCursor::Audio(view.selected_audio)
        } else {
            MediaOptionsCursor::Play
        };
    }

    /// Drop the media-options view (e.g. on Back).
    pub fn close_media_options_view(&mut self) {
        self.media_options_view = None;
    }

    fn handle_media_options_key(&mut self, key: KeyEvent) {
        let Some(view) = self.media_options_view.as_mut() else {
            return;
        };
        if view.loading {
            if matches!(key.code, KeyCode::Esc) {
                self.media_options_view = None;
                self.focus = Pane::LibraryItems;
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.media_options_view = None;
                self.focus = Pane::LibraryItems;
            }
            KeyCode::Backspace => {
                self.media_options_view = None;
                self.focus = Pane::LibraryItems;
            }
            KeyCode::Up => view.cursor = move_options_cursor(view, -1),
            KeyCode::Down => view.cursor = move_options_cursor(view, 1),
            KeyCode::Enter => self.commit_media_options_cursor(),
            _ => {}
        }
    }

    fn commit_media_options_cursor(&mut self) {
        let view = match self.media_options_view.as_mut() {
            Some(v) => v,
            None => return,
        };
        match view.cursor {
            MediaOptionsCursor::Version(n) => {
                view.selected_version = n;
            }
            MediaOptionsCursor::Audio(n) => {
                view.selected_audio = n;
            }
            MediaOptionsCursor::Subtitle(n) => {
                view.selected_subtitle = n;
            }
            MediaOptionsCursor::Chapter(n) => {
                let start_ticks = view
                    .chapters
                    .get(n)
                    .map(|c| c.start_position_ticks)
                    .unwrap_or(0);
                let item_id = view.item_id.clone();
                let item_name = view.item_name.clone();
                let audio = view
                    .audio_entries
                    .get(view.selected_audio)
                    .map(|e| e.choice)
                    .unwrap_or(TrackChoice::Auto);
                let subtitle = view
                    .subtitle_entries
                    .get(view.selected_subtitle)
                    .map(|e| e.choice)
                    .unwrap_or(TrackChoice::Auto);
                let media_source_id = view
                    .versions
                    .get(view.selected_version)
                    .map(|v| v.source_id.clone());
                let selection = VideoTrackSelection {
                    audio,
                    subtitle,
                    media_source_id,
                };
                self.video_track_selections
                    .insert(item_id.clone(), selection);
                self.media_options_view = None;
                self.focus = Pane::LibraryItems;
                let item = Item {
                    id: item_id,
                    name: item_name,
                    kind: Some("Movie".to_string()),
                    ..Default::default()
                };
                self.pending.push(Intent::Play {
                    item,
                    media: MediaKind::Video,
                    start_ticks: Some(start_ticks),
                });
            }
            MediaOptionsCursor::WatchTrailer => {
                let url = view.trailer_urls.first().cloned();
                let title = view.item_name.clone();
                if let Some(url) = url {
                    self.set_status(format!("Trailer: {title}"));
                    self.pending.push(Intent::WatchTrailer { url, title });
                }
            }
            MediaOptionsCursor::Play => {
                let item_id = view.item_id.clone();
                let item_name = view.item_name.clone();
                let audio = view
                    .audio_entries
                    .get(view.selected_audio)
                    .map(|e| e.choice)
                    .unwrap_or(TrackChoice::Auto);
                let subtitle = view
                    .subtitle_entries
                    .get(view.selected_subtitle)
                    .map(|e| e.choice)
                    .unwrap_or(TrackChoice::Auto);
                let media_source_id = view
                    .versions
                    .get(view.selected_version)
                    .map(|v| v.source_id.clone());
                let selection = VideoTrackSelection {
                    audio,
                    subtitle,
                    media_source_id,
                };
                self.video_track_selections
                    .insert(item_id.clone(), selection);
                self.media_options_view = None;
                self.focus = Pane::LibraryItems;
                let item = Item {
                    id: item_id,
                    name: item_name,
                    kind: Some("Movie".to_string()),
                    ..Default::default()
                };
                self.pending.push(Intent::Play {
                    item,
                    media: MediaKind::Video,
                    start_ticks: None,
                });
            }
        }
    }

    /// Push a loading level for `item`, queue the fetch of its children, and
    /// move focus to the middle pane where the children will render. Also
    /// reveals the item so the right-top info pane auto-fetches.
    ///
    /// Focus always shifts to the Content pane so Up/Down + Right walk the
    /// just-opened folder's children without a second keystroke.
    fn drill_into(&mut self, item: &Item) {
        self.stack.push(Level {
            title: item.name.clone(),
            parent_id: item.id.clone(),
            items: Vec::new(),
            selected: 0,
            loading: true,
            parent_item: Some(item.clone()),
            cursor_engaged: false,
        });
        self.focus = Pane::Content;
        self.reveal_item(item.id.clone());
        self.set_context_view(ContextTopView::Info);
        self.pending.push(Intent::LoadCurrentDetail {
            item_id: item.id.clone(),
            kind: item.kind.clone(),
        });
        self.pending.push(Intent::OpenFolder {
            id: item.id.clone(),
            title: item.name.clone(),
        });
    }

    /// Go up one drill level; at a library root, drop focus to the top bar.
    /// Popping the last drilled level while the middle pane has focus returns
    /// focus to the left items pane so the user lands back on the parent list.
    fn go_back(&mut self) {
        if self.is_drilled() {
            self.stack.pop();
            if !self.is_drilled() && self.focus == Pane::Content {
                self.focus = Pane::LibraryItems;
            }
        } else {
            self.focus = Pane::TopBar;
        }
    }

    /// Switch to library at `index` (top-bar 1-9 keys). No-op when out of range.
    pub fn select_library(&mut self, index: usize) {
        if index < self.libraries.len() && index != self.library_selected {
            self.library_selected = index;
            self.reset_stack_for_library();
            // Save the new active library so the next launch starts here.
            if let Some(library) = self.current_library() {
                let id = library.id.clone();
                self.pending.push(Intent::SaveLastLibrary(id));
            }
        }
    }

    /// Drain the queued side effects for the event loop to perform.
    pub fn take_intents(&mut self) -> Vec<Intent> {
        std::mem::take(&mut self.pending)
    }

    /// Push a side-effect onto the pending queue. Used by collaborators (e.g.
    /// [`Playback`]) that need to schedule follow-up work back through the
    /// main intent loop.
    pub fn queue_intent(&mut self, intent: Intent) {
        self.pending.push(intent);
    }

    /// Set the transient status-bar note (used by the loop for playback feedback).
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
    }

    fn cursor_down(&mut self) {
        match self.focus {
            Pane::LibraryItems => {
                if let Some(level) = self.stack.first_mut() {
                    if level.selected + 1 < level.items.len() {
                        level.selected += 1;
                    }
                }
            }
            Pane::Content => {
                if self.is_drilled() {
                    if let Some(level) = self.stack.last_mut() {
                        level.cursor_engaged = true;
                        if level.selected + 1 < level.items.len() {
                            level.selected += 1;
                        }
                    }
                }
            }
            Pane::LibrarySections => {
                let max = self.current_sections().len().saturating_sub(1);
                if self.section_selected < max {
                    self.section_selected += 1;
                }
            }
            Pane::ContextTop => self.scroll_focused_context(1),
            Pane::ContextBottom => self.scroll_focused_context(1),
            Pane::TopBar => {}
            Pane::Home => self.move_home_cursor(1, 0),
            Pane::GlobalSearch => {}
        }
    }

    fn cursor_up(&mut self) {
        match self.focus {
            Pane::LibraryItems => {
                if let Some(level) = self.stack.first_mut() {
                    level.selected = level.selected.saturating_sub(1);
                }
            }
            Pane::Content => {
                if self.is_drilled() {
                    if let Some(level) = self.stack.last_mut() {
                        level.cursor_engaged = true;
                        level.selected = level.selected.saturating_sub(1);
                    }
                }
            }
            Pane::LibrarySections => {
                self.section_selected = self.section_selected.saturating_sub(1);
            }
            Pane::ContextTop => self.scroll_focused_context(-1),
            Pane::ContextBottom => self.scroll_focused_context(-1),
            Pane::TopBar => {}
            Pane::Home => self.move_home_cursor(-1, 0),
            Pane::GlobalSearch => {}
        }
    }

    fn go_top(&mut self) {
        match self.focus {
            Pane::LibraryItems => {
                if let Some(level) = self.stack.first_mut() {
                    level.selected = 0;
                }
            }
            Pane::Content => {
                if self.is_drilled() {
                    if let Some(level) = self.stack.last_mut() {
                        level.cursor_engaged = true;
                        level.selected = 0;
                    }
                }
            }
            Pane::LibrarySections => self.section_selected = 0,
            _ => {}
        }
    }

    fn go_bottom(&mut self) {
        match self.focus {
            Pane::LibraryItems => {
                if let Some(level) = self.stack.first_mut() {
                    level.selected = level.items.len().saturating_sub(1);
                }
            }
            Pane::Content => {
                if self.is_drilled() {
                    if let Some(level) = self.stack.last_mut() {
                        level.cursor_engaged = true;
                        level.selected = level.items.len().saturating_sub(1);
                    }
                }
            }
            Pane::LibrarySections => {
                self.section_selected = self.current_sections().len().saturating_sub(1);
            }
            _ => {}
        }
    }

    /// Panes participating in Tab / Shift+Tab cycling, in left-to-right order.
    /// Context panes are filtered out when they would render an empty
    /// placeholder, so Tab never lands on a dead pane. Home and GlobalSearch
    /// are full-screen views and intentionally not part of the cycle.
    fn pane_cycle(&self) -> Vec<Pane> {
        let mut cycle = vec![
            Pane::TopBar,
            Pane::LibraryItems,
            Pane::LibrarySections,
            Pane::Content,
        ];
        if self.is_context_top_active() {
            cycle.push(Pane::ContextTop);
        }
        if self.is_context_bottom_active() {
            cycle.push(Pane::ContextBottom);
        }
        cycle
    }

    /// True when the top context pane has real content to show — used by
    /// `pane_cycle` so Tab skips empty placeholders.
    fn is_context_top_active(&self) -> bool {
        let collection = self
            .current_library()
            .and_then(|l| l.collection_type.as_deref());
        match collection {
            Some("music") => true,
            Some("movies") | Some("tvshows") => self.current_item_revealed(),
            _ => false,
        }
    }

    /// True when the bottom context pane has real content to show.
    fn is_context_bottom_active(&self) -> bool {
        let collection = self
            .current_library()
            .and_then(|l| l.collection_type.as_deref());
        match collection {
            Some("music") => !self.queue.is_empty(),
            Some("movies") | Some("tvshows") => self.current_item_revealed(),
            _ => false,
        }
    }

    /// Cycle focus to the next pane (Tab). Wraps around to the first pane.
    fn focus_next(&mut self) {
        let cycle = self.pane_cycle();
        let i = cycle
            .iter()
            .position(|p| *p == self.focus)
            .unwrap_or(usize::MAX);
        self.focus = if i == usize::MAX {
            cycle[0]
        } else {
            cycle[(i + 1) % cycle.len()]
        };
    }

    /// Cycle focus to the previous pane (Shift+Tab). Wraps around to the last pane.
    fn focus_prev(&mut self) {
        let cycle = self.pane_cycle();
        let i = cycle
            .iter()
            .position(|p| *p == self.focus)
            .unwrap_or(usize::MAX);
        self.focus = if i == usize::MAX {
            cycle[cycle.len() - 1]
        } else {
            cycle[(i + cycle.len() - 1) % cycle.len()]
        };
    }

    /// Flip the focused item's favorite state and queue a server update.
    fn toggle_favorite(&mut self) {
        let Some(level) = self.current_level_mut() else {
            return;
        };
        let Some(item) = level.items.get_mut(level.selected) else {
            return;
        };
        item.is_favorite = !item.is_favorite;
        let (item_id, favorite, name) = (item.id.clone(), item.is_favorite, item.name.clone());
        self.status_message = Some(if favorite {
            format!("Favorited: {name}")
        } else {
            format!("Unfavorited: {name}")
        });
        self.pending.push(Intent::SetFavorite { item_id, favorite });
    }

    /// Revert an optimistic favorite toggle when the server call fails.
    pub fn revert_favorite(&mut self, item_id: &str, favorite: bool) {
        for level in &mut self.stack {
            for item in &mut level.items {
                if item.id == item_id {
                    item.is_favorite = favorite;
                }
            }
        }
    }

    /// Flip the focused item's played state and queue a server update.
    fn toggle_played(&mut self) {
        let Some(level) = self.current_level_mut() else {
            return;
        };
        let Some(item) = level.items.get_mut(level.selected) else {
            return;
        };
        item.is_played = !item.is_played;
        let (item_id, played, name) = (item.id.clone(), item.is_played, item.name.clone());
        self.status_message = Some(if played {
            format!("Marked played: {name}")
        } else {
            format!("Marked unplayed: {name}")
        });
        self.pending.push(Intent::SetPlayed { item_id, played });
    }

    /// Revert an optimistic played toggle when the server call fails.
    pub fn revert_played(&mut self, item_id: &str, played: bool) {
        for level in &mut self.stack {
            for item in &mut level.items {
                if item.id == item_id {
                    item.is_played = played;
                }
            }
        }
    }
}

/// Run the main browser UI loop until the user quits.
///
/// The loop is tick-driven (a short input poll, then a redraw) so the
/// now-playing bar advances and playback bookkeeping runs even when the user
/// isn't pressing keys. `playback` is `None` only when there are no
/// credentials, so the browser still runs to show the error.
pub(crate) fn run_browser(
    terminal: &mut Tui,
    app: &mut App,
    mut playback: Option<&mut super::playback::Playback>,
    mut browser: Option<&mut super::browse::Browser>,
    mut images: Option<&mut super::images::Images>,
    mut details: Option<&mut super::details::Details>,
) -> Result<()> {
    const TICK: std::time::Duration = std::time::Duration::from_millis(200);
    // Cover + detail fetches are gated on selection stability so rapid
    // scrolling doesn't queue a request per item the user blew past. The
    // gate trips once the selection has been steady for `STABLE_TICKS` frames
    // (~200 ms each).
    const STABLE_TICKS: u8 = 2;
    let mut last_item_id: Option<String> = None;
    let mut stable_ticks: u8 = 0;
    while !app.should_quit {
        // Snapshot the current selection so we can run the stability gate
        // without holding any borrows on `app`.
        let current = app
            .current_item()
            .map(|item| (item.id.clone(), item.primary_image_tag.is_some()));
        let current_id = current.as_ref().map(|(id, _)| id.clone());
        let has_art = current.as_ref().is_some_and(|(_, art)| *art);

        if current_id == last_item_id {
            stable_ticks = stable_ticks.saturating_add(1);
        } else {
            // Selection moved — keep the previously-cached detail in memory
            // so the info pane doesn't blank between hovers. The cache is
            // keyed by item id, so the renderer naturally falls back to "no
            // detail yet" until the new item's fetch lands; old detail will
            // never be rendered for the new item. We also auto-reveal the new
            // item so the info pane stays drawn instead of dropping back to
            // its placeholder.
            if let Some(id) = current_id.as_deref() {
                app.reveal_item(id.to_string());
            }
            // Selection-specific transient state that shouldn't follow the
            // user to a new item: scroll offsets, the media-options view.
            // Cached detail itself is intentionally kept — the renderer
            // filters by id so old data never bleeds into the new selection.
            app.reset_context_scroll_for_selection_change();
            stable_ticks = 0;
            last_item_id = current_id.clone();
        }
        let gate_open = stable_ticks >= STABLE_TICKS;

        // Once the selection has been stable for a few ticks, auto-fetch its
        // detail so navigating the list keeps the info pane up-to-date
        // without a manual `p → Load info` step. The `Details` fetcher
        // dedupes per item id, so this won't spam the server.
        if gate_open {
            if let (Some(item), Some(dt)) = (app.current_item().cloned(), details.as_deref_mut())
            {
                dt.request(&item.id, item.kind.as_deref());
            }
        }

        let revealed = app.current_item_revealed();
        if let Some(im) = images.as_deref_mut() {
            im.tick();
            // Covers no longer auto-load on hover. The middle-pane cover is
            // fetched once the user picks "Load info"; the now-playing cover
            // stays unconditional so the bar always shows art while a track plays.
            if gate_open && has_art && revealed {
                if let Some(id) = &current_id {
                    im.request(id);
                }
            }
            if let Some(np) = &app.now_playing {
                im.request(&np.item_id);
            }
        }

        // Detail fetches no longer fire on hover — they're driven by the
        // per-item popup menu's "Load info" action via `Intent::LoadCurrentDetail`.
        if let Some(dt) = details.as_deref_mut() {
            dt.tick(app);
        }

        terminal.draw(|frame| render(frame, app, images.as_deref_mut()))?;
        // Debug affordance to verify crash handling end-to-end (panic while the
        // alternate screen + raw mode are active).
        if std::env::var_os("AQUAFIN_DEBUG_PANIC").is_some() {
            panic!("forced panic for crash-handling test (AQUAFIN_DEBUG_PANIC)");
        }
        // Key events drive state; resize and others just fall through to a redraw.
        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }
        // Folder drilling goes to the browser; theme switches the loop handles
        // directly; everything else is playback.
        for intent in app.take_intents() {
            match intent {
                Intent::OpenFolder { id, .. } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.open(id);
                    }
                }
                Intent::ApplySection {
                    library_id,
                    section,
                    extras,
                } => {
                    let library_name = app
                        .libraries
                        .iter()
                        .find(|l| l.id == library_id)
                        .map(|l| l.name.clone())
                        .unwrap_or_default();
                    if let Some(br) = browser.as_deref_mut() {
                        br.apply_section(library_id, library_name, section, extras);
                    }
                }
                Intent::Search {
                    query,
                    item_types,
                    scope_label,
                } => {
                    let library_id = app
                        .current_library()
                        .map(|l| l.id.clone())
                        .unwrap_or_default();
                    if !library_id.is_empty() {
                        if let Some(br) = browser.as_deref_mut() {
                            br.search(library_id, query, item_types, scope_label);
                        }
                    }
                }
                Intent::SetTheme(name) => match crate::theme::load(&name) {
                    Ok(theme) => {
                        app.set_theme(theme);
                        if let Err(e) = persist_theme(&name) {
                            tracing::warn!(error = %e, "couldn't persist theme");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(theme = %name, error = %e, "couldn't load theme");
                        app.show_error(format!("Couldn't load theme \"{name}\": {e}"));
                    }
                },
                Intent::SaveAudioPrefs { repeat_mode, shuffle } => {
                    if let Err(e) = persist_audio_prefs(repeat_mode, shuffle) {
                        tracing::warn!(error = %e, "couldn't persist audio prefs");
                    }
                }
                Intent::SetGapless(on) => {
                    if let Err(e) = persist_gapless(on) {
                        tracing::warn!(error = %e, "couldn't persist gapless");
                    }
                    if let Some(pb) = playback.as_deref_mut() {
                        pb.set_gapless(on);
                    }
                }
                Intent::SetNormalization(on) => {
                    if let Err(e) = persist_normalization(on) {
                        tracing::warn!(error = %e, "couldn't persist normalization");
                    }
                    if let Some(pb) = playback.as_deref_mut() {
                        pb.set_normalization(on);
                    }
                }
                Intent::SetEqPreset(preset) => {
                    if let Err(e) = persist_eq_preset(preset) {
                        tracing::warn!(error = %e, "couldn't persist EQ preset");
                    }
                    if let Some(pb) = playback.as_deref_mut() {
                        pb.set_eq_preset(preset);
                    }
                }
                Intent::SaveVolume(volume) => {
                    if let Err(e) = persist_volume(volume) {
                        tracing::warn!(error = %e, "couldn't persist volume");
                    }
                }
                Intent::SaveSectionMemory(memory) => {
                    if let Err(e) = persist_section_memory(memory) {
                        tracing::warn!(error = %e, "couldn't persist section memory");
                    }
                }
                Intent::SaveLastLibrary(id) => {
                    if let Err(e) = persist_last_library(id) {
                        tracing::warn!(error = %e, "couldn't persist last library");
                    }
                }
                Intent::SaveSearchHistory(history) => {
                    if let Err(e) = persist_search_history(history) {
                        tracing::warn!(error = %e, "couldn't persist search history");
                    }
                }
                Intent::LoadCurrentDetail { item_id, kind } => {
                    if let Some(dt) = details.as_deref_mut() {
                        dt.request(&item_id, kind.as_deref());
                    }
                    app.reveal_item(item_id);
                    app.set_status("Loading info…");
                }
                Intent::SyncLibraries => {
                    if let Some(br) = browser.as_deref_mut() {
                        let visible = current_visible_libraries();
                        br.sync_libraries(visible);
                    }
                }
                Intent::LoadAllLibraryMeta => {
                    if let Some(br) = browser.as_deref_mut() {
                        let visible = current_visible_libraries();
                        br.load_library_meta(visible);
                    }
                }
                Intent::SaveVisibleLibraries(visible_ids) => {
                    if let Err(e) = persist_visible_libraries(visible_ids.clone()) {
                        tracing::warn!(error = %e, "couldn't persist visible libraries");
                    }
                    if let Some(br) = browser.as_deref_mut() {
                        br.sync_libraries(Some(visible_ids));
                    }
                }
                Intent::LoadPlaylists { .. } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.load_playlists();
                    }
                }
                Intent::AddToPlaylist { playlist_id, item_id } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.add_to_playlist(playlist_id, item_id);
                    }
                }
                Intent::InstantMix { item } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.instant_mix(item);
                    }
                }
                Intent::GenreRadio { library_id, genre } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.genre_radio(library_id, genre);
                    }
                }
                Intent::CreatePlaylist { name, item_ids } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.create_playlist(name, item_ids);
                    }
                }
                Intent::Dislike { item_id } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.dislike(item_id);
                    }
                }
                Intent::CopyItemUrl { item_id, item_name } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.copy_item_url(item_id, item_name);
                    }
                }
                Intent::LoadVideoTracks { item_id, kind } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.load_video_tracks(item_id, kind);
                    }
                }
                Intent::LoadMediaOptions { item_id } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.load_media_options(item_id);
                    }
                }
                Intent::LoadHome => {
                    if let Some(br) = browser.as_deref_mut() {
                        let libs: Vec<(String, String, Option<String>)> = app
                            .libraries
                            .iter()
                            .map(|l| (l.id.clone(), l.name.clone(), l.collection_type.clone()))
                            .collect();
                        br.load_home(libs);
                    }
                }
                Intent::GlobalSearch { query } => {
                    if let Some(br) = browser.as_deref_mut() {
                        br.global_search(query);
                    }
                }
                other => {
                    if let Some(pb) = playback.as_deref_mut() {
                        pb.dispatch(other, app);
                    }
                }
            }
        }
        if let Some(br) = browser.as_deref_mut() {
            br.tick(app);
        }
        if let Some(pb) = playback.as_deref_mut() {
            pb.tick(app);
        }
    }
    Ok(())
}

pub(crate) fn init_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

pub(crate) fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Read-modify-write the config file so the user's chosen queue prefs survive
/// a restart. Missing config (e.g. first run before setup) becomes a default
/// config with the new prefs applied — no surprise: setup writes the rest of
/// the file later anyway.
fn persist_audio_prefs(repeat_mode: RepeatMode, shuffle: bool) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.audio.repeat_mode = repeat_mode.into();
    config.audio.shuffle = shuffle;
    config.save()
}

fn persist_gapless(on: bool) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.audio.gapless = on;
    config.save()
}

fn persist_normalization(on: bool) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.audio.normalization = on;
    config.save()
}

fn persist_eq_preset(preset: crate::config::EqPreset) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.audio.eq.preset = preset;
    config.audio.eq.enabled = !matches!(preset, crate::config::EqPreset::Flat);
    config.save()
}

fn persist_volume(volume: u8) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.audio.volume = volume;
    config.save()
}

fn persist_section_memory(
    memory: std::collections::HashMap<String, String>,
) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.ui.section_memory = memory;
    config.save()
}

fn persist_last_library(id: String) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.ui.last_library_id = Some(id);
    config.save()
}

/// Menu label for the "Go to artist" action — `None` when the item has no
/// known primary artist id. Shown on tracks and albums (`MusicAlbum`); a
/// `MusicArtist` is itself the artist, so the action is hidden.
fn go_to_artist_label(item: &Item) -> Option<String> {
    if item.kind.as_deref() == Some("MusicArtist") {
        return None;
    }
    let id = item.primary_artist_id.as_deref()?;
    if id.is_empty() {
        return None;
    }
    let name = item
        .primary_artist_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("artist");
    Some(format!("Go to artist: {name}"))
}

/// Menu label for the "Go to album" action — `None` unless the item has a
/// parent album id (i.e. an `Audio` track).
fn go_to_album_label(item: &Item) -> Option<String> {
    let id = item.album_id.as_deref()?;
    if id.is_empty() {
        return None;
    }
    let name = item
        .album_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("album");
    Some(format!("Go to album: {name}"))
}

/// Format the level title for an in-flight or completed search. The scope
/// label (current section name, e.g. "Albums") is included when non-empty so
/// the user can see the search is narrowed; section "All" yields the bare
/// "Search: …" form.
pub(super) fn search_level_title(scope_label: &str, query: &str) -> String {
    if scope_label.is_empty() {
        format!("Search: {query}")
    } else {
        format!("Search · {scope_label}: {query}")
    }
}

fn persist_search_history(history: Vec<String>) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.ui.search_history = history;
    config.save()
}

fn persist_theme(name: &str) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.ui.theme = name.to_string();
    config.save()
}

fn persist_visible_libraries(visible: Vec<String>) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.ui.visible_libraries = Some(visible);
    config.save()
}

/// Read the persisted visible-libraries list. `None` (the default for a fresh
/// install) means "all libraries visible".
fn current_visible_libraries() -> Option<Vec<String>> {
    crate::config::Config::load()
        .ok()
        .flatten()
        .and_then(|c| c.ui.visible_libraries)
}

pub fn render(frame: &mut Frame, app: &App, mut images: Option<&mut super::images::Images>) {
    let area = frame.area();
    let regions = layout::compute(area);
    let theme = &app.theme;

    panes::top_bar::render(
        frame,
        regions.top_bar,
        &app.libraries,
        app.library_selected,
        app.search_query.as_deref(),
        app.focus == Pane::TopBar,
        app.is_on_home(),
        app.is_on_global_search(),
        theme,
    );

    // Global search takes over the main area, like Home.
    if app.is_on_global_search() {
        panes::global_search::render(
            frame,
            regions.main,
            app.global_search_state(),
            app.focus == Pane::GlobalSearch,
            theme,
        );
        super::now_playing::render(
            frame,
            regions.now_playing,
            app.now_playing.as_ref(),
            images,
            theme,
        );
        render_status(frame, regions.status, app);
        render_overlays(frame, area, app, theme);
        return;
    }

    // Home takes over the full main band; the right-side context panes and
    // middle content pane don't render in this mode.
    if app.is_on_home() {
        let rows = app.home_rows();
        panes::home::render(
            frame,
            regions.main,
            app.home_data(),
            &rows,
            app.home_cursor(),
            None,
            app.focus == Pane::Home,
            theme,
        );
        super::now_playing::render(
            frame,
            regions.now_playing,
            app.now_playing.as_ref(),
            images,
            theme,
        );
        render_status(frame, regions.status, app);
        render_overlays(frame, area, app, theme);
        return;
    }

    let root_level = app.root_level();
    let root_title = root_level.map(|l| l.title.clone()).unwrap_or_default();
    panes::library_items::render(
        frame,
        regions.library_items,
        root_level,
        &root_title,
        app.focus == Pane::LibraryItems,
        theme,
    );

    let sections = app.current_sections();
    panes::library_sections::render(
        frame,
        regions.library_sections,
        &sections,
        app.section_selected,
        app.focus == Pane::LibrarySections,
        theme,
    );

    if let Some(view) = app.media_options_view() {
        panes::content::render_media_options(
            frame,
            regions.content,
            view,
            app.focus == Pane::Content,
            theme,
        );
    } else {
        panes::content::render(
            frame,
            regions.content,
            app.drilled_level(),
            app.focus == Pane::Content,
            theme,
        );
    }

    let collection_type = app.current_library().and_then(|l| l.collection_type.as_deref());
    let detail = app.current_detail();
    let playback_position = app.now_playing.as_ref().map(|np| np.position);
    panes::context_pane::render_top(
        frame,
        regions.context_top,
        collection_type,
        app.current_item(),
        detail,
        app.context_view(),
        app.context_top_scroll(),
        app.current_item_revealed(),
        playback_position,
        images.as_deref_mut(),
        app.focus == Pane::ContextTop,
        theme,
    );
    panes::context_pane::render_bottom(
        frame,
        regions.context_bottom,
        collection_type,
        detail,
        app.now_playing.as_ref(),
        app.current_queue_track(),
        app.upcoming_queue(),
        app.repeat_mode,
        app.shuffle,
        app.context_bottom_scroll(),
        app.focus == Pane::ContextBottom,
        theme,
    );

    super::now_playing::render(
        frame,
        regions.now_playing,
        app.now_playing.as_ref(),
        images,
        theme,
    );

    render_status(frame, regions.status, app);

    render_overlays(frame, area, app, theme);
}

/// Render any overlay that sits on top of the main content (error modal,
/// theme picker, library/playlist/video-track pickers, popup menu, help
/// cheatsheet). Shared by every top-level view so opening a popup from Home
/// or Search still renders.
fn render_overlays(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if let Some(message) = &app.error {
        let log_location = crate::paths::state_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        error_modal::render(
            frame,
            area,
            message,
            &log_location,
            app.error_copied,
            theme,
        );
    } else if let Some((names, selected)) = app.theme_picker() {
        theme_picker::render(frame, area, names, selected, theme.name(), theme);
    } else if let Some(picker) = app.library_picker() {
        let (title, entries, selected) = render_library_picker_data(picker);
        let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
        popup_menu::render(frame, area, title, &refs, selected, theme);
    } else if let Some(picker) = app.playlist_picker() {
        let (title, entries, selected) = render_playlist_picker_data(picker);
        let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
        popup_menu::render(frame, area, title, &refs, selected, theme);
    } else if let Some(picker) = app.video_track_picker() {
        let (title, entries, selected) = render_video_track_picker_data(picker);
        let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
        popup_menu::render(frame, area, title, &refs, selected, theme);
    } else if let Some((title, entries, selected)) = app.popup_menu() {
        let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
        popup_menu::render(frame, area, title, &refs, selected, theme);
    } else if app.show_help {
        cheatsheet::render(frame, area, &app.keymap, theme);
    }
}

/// Materialise the visible-library picker into the (title, entries, selected)
/// shape `popup_menu::render` expects.
fn render_library_picker_data(picker: &LibraryPickerState) -> (&'static str, Vec<String>, usize) {
    if picker.loading {
        return (
            "Visible libraries — Space toggle, Enter save, Esc cancel",
            vec!["Loading…".to_string()],
            0,
        );
    }
    if picker.entries.is_empty() {
        return (
            "Visible libraries",
            vec!["No libraries on server".to_string()],
            0,
        );
    }
    let entries = picker
        .entries
        .iter()
        .map(|(_, name, on)| format!("[{}] {}", if *on { "x" } else { " " }, name))
        .collect();
    (
        "Visible libraries — Space toggle, Enter save, Esc cancel",
        entries,
        picker.selected,
    )
}

/// Move the media-options cursor `delta` rows up (-1) or down (+1), skipping
/// non-selectable rows and clamping at the ends.
fn move_options_cursor(view: &MediaOptionsViewState, delta: i32) -> MediaOptionsCursor {
    let positions = options_cursor_positions(view);
    if positions.is_empty() {
        return MediaOptionsCursor::Play;
    }
    let current = positions
        .iter()
        .position(|c| *c == view.cursor)
        .unwrap_or(0);
    let next = (current as i32 + delta)
        .clamp(0, positions.len() as i32 - 1) as usize;
    positions[next]
}

/// Linearised list of selectable rows in the media-options view, in render
/// order. Used for keyboard navigation.
pub(crate) fn options_cursor_positions(view: &MediaOptionsViewState) -> Vec<MediaOptionsCursor> {
    let mut positions = Vec::new();
    for i in 0..view.versions.len() {
        positions.push(MediaOptionsCursor::Version(i));
    }
    for i in 0..view.audio_entries.len() {
        positions.push(MediaOptionsCursor::Audio(i));
    }
    for i in 0..view.subtitle_entries.len() {
        positions.push(MediaOptionsCursor::Subtitle(i));
    }
    positions.push(MediaOptionsCursor::Play);
    if !view.trailer_urls.is_empty() {
        positions.push(MediaOptionsCursor::WatchTrailer);
    }
    for i in 0..view.chapters.len() {
        positions.push(MediaOptionsCursor::Chapter(i));
    }
    positions
}

/// Cheap timestamp label for the auto-generated playlist name. Format
/// `YYYY-MM-DD HH:MM` based on the wall-clock UTC seconds since epoch — good
/// enough for a sortable, human-readable handle without pulling in `chrono`.
fn chrono_like_now_label() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let (year, month, day) = days_to_ymd(days);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}")
}

/// Convert "days since 1970-01-01" to a `(year, month, day)` tuple using the
/// Howard Hinnant algorithm. Pure integer math, valid for all positive epochs.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m as u32, d as u32)
}

/// Cycle through the runtime sort presets. `None` means "use section default".
fn next_sort_override(current: Option<&[String]>) -> Option<Vec<String>> {
    const ORDER: &[&str] = &[
        "SortName",
        "DateCreated",
        "ProductionYear",
        "CommunityRating",
        "PlayCount",
        "Random",
        "Runtime",
    ];
    let current_first = current.and_then(|s| s.first()).map(String::as_str);
    let idx = current_first.and_then(|n| ORDER.iter().position(|p| *p == n));
    match idx {
        None => Some(vec![ORDER[0].to_string()]),
        Some(i) if i + 1 < ORDER.len() => Some(vec![ORDER[i + 1].to_string()]),
        Some(_) => None, // wrap back to "Section default"
    }
}

/// Label for the sleep-timer entry in the item menu: shows remaining time when
/// armed, "End of track" with the EOT mode, or just "Off".
fn sleep_timer_menu_label(state: SleepTimer, remaining: Option<u64>) -> String {
    match state {
        SleepTimer::Off => "Sleep timer: Off".to_string(),
        SleepTimer::EndOfTrack => "Sleep timer: End of track".to_string(),
        other => match remaining {
            Some(secs) => format!("Sleep timer: {} ({} left)", other.label(), format_remaining(secs)),
            None => format!("Sleep timer: {}", other.label()),
        },
    }
}

/// Render a remaining-seconds count as `Hh Mm` / `Mm Ss` / `Ss`.
fn format_remaining(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

fn eq_preset_label(p: crate::config::EqPreset) -> &'static str {
    use crate::config::EqPreset::*;
    match p {
        Flat => "Flat",
        BassBoost => "Bass boost",
        Vocal => "Vocal",
        TrebleBoost => "Treble boost",
        Custom => "Custom",
    }
}

fn cycle_eq_preset(p: crate::config::EqPreset) -> crate::config::EqPreset {
    use crate::config::EqPreset::*;
    match p {
        Flat => BassBoost,
        BassBoost => Vocal,
        Vocal => TrebleBoost,
        TrebleBoost => Flat,
        Custom => Flat,
    }
}

/// Short label for a `TrackChoice`, used by the item-menu summary line.
fn track_choice_label(choice: TrackChoice) -> String {
    match choice {
        TrackChoice::Auto => "Auto".to_string(),
        TrackChoice::Off => "Off".to_string(),
        TrackChoice::Pick(n) => format!("#{n}"),
    }
}

/// Materialise the video track picker into the shape `popup_menu::render`
/// expects.
fn render_video_track_picker_data(
    picker: &VideoTrackPickerState,
) -> (&'static str, Vec<String>, usize) {
    let title = match picker.kind {
        VideoTrackKind::Audio => "Audio track",
        VideoTrackKind::Subtitle => "Subtitles",
    };
    match picker.entries.as_ref() {
        None => (title, vec!["Loading…".to_string()], 0),
        Some(entries) if entries.is_empty() => (
            title,
            vec!["No tracks reported by server".to_string()],
            0,
        ),
        Some(entries) => (
            title,
            entries.iter().map(|e| e.label.clone()).collect(),
            picker.selected,
        ),
    }
}

/// Materialise the playlist picker into the (title, entries, selected) shape
/// `popup_menu::render` expects.
fn render_playlist_picker_data(picker: &PlaylistPickerState) -> (&'static str, Vec<String>, usize) {
    match picker.entries.as_ref() {
        None => ("Add to playlist", vec!["Loading…".to_string()], 0),
        Some(entries) if entries.is_empty() => (
            "Add to playlist",
            vec!["No playlists on server".to_string()],
            0,
        ),
        Some(entries) => (
            "Add to playlist",
            entries.iter().map(|(_, name)| name.clone()).collect(),
            picker.selected,
        ),
    }
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    if app.awaiting_go_to() {
        let line = " Go to: h Home · s Search · t Top · i Items · c Content · n Info · l Lyrics · q Queue · ? Help · Esc cancel ";
        frame.render_widget(Paragraph::new(line).style(app.theme.header()), area);
        return;
    }
    let focus_name = match app.focus {
        Pane::TopBar => "Libraries",
        Pane::LibraryItems => "Items",
        Pane::LibrarySections => "Sections",
        Pane::Content => "Details",
        Pane::ContextTop => "Context",
        Pane::ContextBottom => "Queue",
        Pane::Home => "Home",
        Pane::GlobalSearch => "Search",
    };
    // A transient note (e.g. "Not playable") takes over the left side when set.
    let left = match &app.status_message {
        Some(message) => format!(" {message} "),
        None => match app.focus {
            Pane::Home => " Home  ·  dashboard ".to_string(),
            Pane::GlobalSearch => {
                let q = app.global_search_state().query.clone();
                if q.is_empty() {
                    " Search  ·  global ".to_string()
                } else {
                    format!(" Search  ·  global  ·  \"{q}\" ")
                }
            }
            _ => {
                let item = app.current_item().map_or("-", |i| i.name.as_str());
                format!(" {focus_name}  ·  {}  ·  {item} ", app.breadcrumb())
            }
        },
    };
    let hint = " Enter open/play · Bksp back · g go to · F1 help · q quit ";

    let [left_area, right_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(hint.chars().count() as u16),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(left).style(app.theme.status_bar()), left_area);
    frame.render_widget(
        Paragraph::new(hint)
            .style(app.theme.hint())
            .alignment(Alignment::Right),
        right_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn rendered(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app, None)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                if let Some(cell) = buffer.cell((x, y)) {
                    out.push_str(cell.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn tab_cycles_focus_with_wrap_around() {
        // Tab walks forward across active panes; Shift+Tab walks back. Both
        // wrap. Inactive context panes (no revealed item, empty queue) are
        // skipped so Tab never lands on a dead placeholder.
        let mut app = App::new();
        assert_eq!(app.focus, Pane::LibraryItems);
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.focus, Pane::LibrarySections);
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Content);
        // Context panes are inactive without a revealed item / queued track,
        // so Tab wraps back to the TopBar.
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.focus, Pane::TopBar);
        app.handle_key(press(KeyCode::Tab));
        assert_eq!(app.focus, Pane::LibraryItems);
        // Shift+Tab wraps the other direction.
        app.handle_key(press(KeyCode::BackTab));
        assert_eq!(app.focus, Pane::TopBar);
        app.handle_key(press(KeyCode::BackTab));
        assert_eq!(app.focus, Pane::Content);
    }

    #[test]
    fn left_right_arrows_act_as_back_and_activate() {
        // Right activates (like Enter); Left goes back (like Backspace).
        // Pane cycling is reserved for Tab / Shift+Tab.
        let mut app = App::new();
        assert_eq!(app.focus, Pane::LibraryItems);
        // From a non-drilled library root, Left walks up to the top bar.
        app.handle_key(press(KeyCode::Left));
        assert_eq!(app.focus, Pane::TopBar);
    }

    #[test]
    fn arrow_keys_move_selection_in_library_items() {
        // Focus starts on LibraryItems, so Down/Up walk the items list.
        let mut app = App::new();
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.current_level().unwrap().selected, 1);
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.current_level().unwrap().selected, 0);
        app.handle_key(press(KeyCode::Up)); // clamps at 0
        assert_eq!(app.current_level().unwrap().selected, 0);
    }

    #[test]
    fn digit_keys_switch_library() {
        // Libraries are selected from the top bar via 1-9.
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('2')));
        assert_eq!(app.library_selected, 1);
        app.handle_key(press(KeyCode::Char('3')));
        assert_eq!(app.library_selected, 2);
        // Out-of-range digit is a no-op.
        app.handle_key(press(KeyCode::Char('9')));
        assert_eq!(app.library_selected, 2);
    }

    #[test]
    fn switching_library_resets_list_cursor() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.current_level().unwrap().selected, 1);
        app.handle_key(press(KeyCode::Char('2')));
        assert_eq!(app.library_selected, 1);
        assert_eq!(app.current_level().unwrap().selected, 0);
    }

    #[test]
    fn home_and_end_jump_top_and_bottom_in_items() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::End));
        let last = app.current_level().unwrap().items.len() - 1;
        assert_eq!(app.current_level().unwrap().selected, last);
        app.handle_key(press(KeyCode::Home));
        assert_eq!(app.current_level().unwrap().selected, 0);
    }

    #[test]
    fn f1_opens_help_and_any_key_closes_it() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::F(1)));
        assert!(app.show_help);
        app.handle_key(press(KeyCode::Down)); // any key closes; should not also move
        assert!(!app.show_help);
        assert_eq!(app.current_level().unwrap().selected, 0);
    }

    #[test]
    fn q_requests_quit() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn space_toggles_pause_intent() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char(' ')));
        assert_eq!(app.take_intents(), vec![Intent::TogglePause]);
    }

    #[test]
    fn f_toggles_favorite_optimistically_and_emits_intent() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Char('f')));
        assert!(app.current_item().unwrap().is_favorite);
        assert_eq!(
            app.take_intents(),
            vec![Intent::SetFavorite { item_id: "id-Thing".into(), favorite: true }]
        );
        app.handle_key(press(KeyCode::Char('f')));
        assert!(!app.current_item().unwrap().is_favorite);
        assert_eq!(
            app.take_intents(),
            vec![Intent::SetFavorite { item_id: "id-Thing".into(), favorite: false }]
        );
    }

    fn typed_item(name: &str, kind: &str) -> Item {
        Item {
            id: format!("id-{name}"),
            name: name.to_string(),
            kind: Some(kind.to_string()),
            is_folder: matches!(
                kind,
                "Series" | "Season" | "MusicAlbum" | "MusicArtist" | "Playlist"
            ),
            ..Default::default()
        }
    }

    fn app_with_item(kind: &str) -> App {
        App::with_libraries(vec![Library {
            id: "lib".to_string(),
            name: "Lib".to_string(),
            collection_type: None,
            items: vec![typed_item("Thing", kind)],
        }])
    }

    #[test]
    fn media_kind_classifies_types() {
        assert_eq!(MediaKind::classify(Some("Movie")), MediaKind::Video);
        assert_eq!(MediaKind::classify(Some("Episode")), MediaKind::Video);
        assert_eq!(MediaKind::classify(Some("Audio")), MediaKind::Audio);
        assert_eq!(MediaKind::classify(Some("Series")), MediaKind::Other);
        assert_eq!(MediaKind::classify(None), MediaKind::Other);
    }

    #[test]
    fn enter_on_video_opens_media_options_view() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Enter));
        assert!(app.media_options_view().is_some());
        assert_eq!(app.focus, Pane::Content);
        let intents = app.take_intents();
        assert!(intents.iter().any(|i| matches!(i, Intent::LoadMediaOptions { .. })));
        assert!(intents.iter().any(|i| matches!(i, Intent::LoadCurrentDetail { .. })));
        assert!(!intents.iter().any(|i| matches!(i, Intent::Play { .. })));
    }

    #[test]
    fn enter_on_audio_plays_directly() {
        let mut app = app_with_item("Audio");
        app.handle_key(press(KeyCode::Enter));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::Play { media: MediaKind::Audio, .. }]
        ));
    }

    #[test]
    fn watch_trailer_row_emits_intent_when_trailer_url_present() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        app.set_media_options_view_data(
            "id-Thing",
            vec![],
            vec![TrackPickerEntry {
                label: "Auto".to_string(),
                choice: TrackChoice::Auto,
            }],
            vec![TrackPickerEntry {
                label: "Auto".to_string(),
                choice: TrackChoice::Auto,
            }],
        );
        app.set_current_detail(
            "id-Thing",
            ItemDetail {
                trailer_urls: vec!["https://youtu.be/abc".to_string()],
                ..Default::default()
            },
        );
        // Cursor starts on Audio(0); navigate to WatchTrailer (last row).
        loop {
            let cursor = app.media_options_view().unwrap().cursor;
            if matches!(cursor, MediaOptionsCursor::WatchTrailer) {
                break;
            }
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::WatchTrailer { url, .. }] if url == "https://youtu.be/abc"
        ));
    }

    #[test]
    fn media_options_play_row_emits_play_with_chosen_options() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        app.set_media_options_view_data(
            "id-Thing",
            vec![MediaVersion {
                source_id: "src-a".to_string(),
                label: "Original".to_string(),
            }],
            vec![
                TrackPickerEntry {
                    label: "Auto".to_string(),
                    choice: TrackChoice::Auto,
                },
                TrackPickerEntry {
                    label: "#1 English".to_string(),
                    choice: TrackChoice::Pick(1),
                },
            ],
            vec![TrackPickerEntry {
                label: "Off".to_string(),
                choice: TrackChoice::Off,
            }],
        );
        // Cursor lands on Audio(0) since there's only one version.
        // Navigate Down to Audio(1), commit (selects audio).
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        // Down → Subtitle(0), commit (selects Off subs).
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        // Down → Play, commit.
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        assert!(app.media_options_view().is_none());
        let intents = app.take_intents();
        let played = intents.iter().any(|i| matches!(i, Intent::Play { .. }));
        assert!(played, "Play intent should be emitted");
        let options = app.video_options_for("id-Thing");
        assert!(matches!(options.audio, Some(TrackChoice::Pick(1))));
        assert!(matches!(options.subtitle, Some(TrackChoice::Off)));
    }

    #[test]
    fn enter_on_unplayable_item_sets_status_not_intent() {
        // A non-folder, non-media item (e.g. a photo): can't play, can't drill.
        let mut app = app_with_item("Photo");
        app.handle_key(press(KeyCode::Enter));
        assert!(app.take_intents().is_empty());
        let out = rendered(&app, 100, 30);
        assert!(out.contains("Not playable: Thing"), "{out}");
    }

    #[test]
    fn enter_on_folder_drills_in_and_queues_open() {
        let mut app = app_with_item("Series"); // a folder
        app.handle_key(press(KeyCode::Enter));
        // A loading level for the folder is pushed immediately…
        assert!(app.is_drilled());
        let level = app.current_level().unwrap();
        assert!(level.loading);
        assert_eq!(level.parent_id, "id-Thing");
        // …and an OpenFolder intent is queued for the loader (plus the
        // auto-reveal that fetches detail for the right-top info pane).
        let intents = app.take_intents();
        assert!(intents.iter().any(|i| matches!(i, Intent::LoadCurrentDetail { .. })));
        assert!(intents
            .iter()
            .any(|i| matches!(i, Intent::OpenFolder { id, .. } if id == "id-Thing")));
    }

    #[test]
    fn fill_level_populates_then_back_pops() {
        let mut app = app_with_item("Series");
        app.handle_key(press(KeyCode::Enter)); // drill in (loading)
        let _ = app.take_intents();
        app.fill_level(
            "id-Thing",
            vec![typed_item("Season 1", "Season"), typed_item("Season 2", "Season")],
        );
        let level = app.current_level().unwrap();
        assert!(!level.loading);
        assert_eq!(level.items.len(), 2);
        assert_eq!(app.breadcrumb(), "Lib › Thing");
        // Backspace walks back up to the library root.
        app.handle_key(press(KeyCode::Backspace));
        assert!(!app.is_drilled());
        assert_eq!(app.current_item().map(|i| i.name.as_str()), Some("Thing"));
    }

    #[test]
    #[test]
    fn enter_on_artist_in_music_library_keeps_current_item_on_artist() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Daft Punk", "MusicArtist")],
        }]);
        app.handle_key(press(KeyCode::Enter));
        assert!(app.is_drilled());
        // Music does shift focus to Content, but the empty loading level
        // exposes its `parent_item`, so the info pane keeps rendering Daft
        // Punk until the artist's albums load.
        assert_eq!(app.focus, Pane::Content);
        assert_eq!(app.current_item().map(|i| i.name.as_str()), Some("Daft Punk"));
        assert!(app.current_item_revealed());
    }

    #[test]
    fn drilled_level_stays_on_parent_until_cursor_moves() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Discovery", "MusicAlbum")],
        }]);
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        // Loader fills the drilled level with tracks.
        let parent_id = app.current_level().unwrap().parent_id.clone();
        app.fill_level(
            &parent_id,
            vec![
                Item { id: "t1".to_string(), name: "Track 1".to_string(), kind: Some("Audio".to_string()), ..Default::default() },
                Item { id: "t2".to_string(), name: "Track 2".to_string(), kind: Some("Audio".to_string()), ..Default::default() },
            ],
        );
        // Before any cursor move, the info pane stays pinned on Discovery.
        assert_eq!(app.current_item().map(|i| i.name.as_str()), Some("Discovery"));
        // A single Down flips cursor_engaged + moves the info pane onto the
        // first track.
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.current_item().map(|i| i.name.as_str()), Some("Track 2"));
    }

    #[test]
    fn enter_on_album_keeps_current_item_on_album_until_tracks_load() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Discovery", "MusicAlbum")],
        }]);
        app.handle_key(press(KeyCode::Enter));
        assert!(app.is_drilled());
        assert_eq!(app.focus, Pane::Content);
        assert_eq!(app.current_item().map(|i| i.name.as_str()), Some("Discovery"));
        assert!(app.current_item_revealed());
    }

    #[test]
    fn enter_on_series_drills_and_shifts_focus_to_content() {
        let mut app = App::with_libraries(vec![Library {
            id: "tv".to_string(),
            name: "TV".to_string(),
            collection_type: Some("tvshows".to_string()),
            items: vec![typed_item("Severance", "Series")],
        }]);
        app.handle_key(press(KeyCode::Enter));
        assert!(app.is_drilled());
        assert_eq!(app.focus, Pane::Content);
        // Until the cursor engages in the drilled level, current_item stays on
        // the parent so the info pane keeps rendering Severance instead of a
        // blank child slot.
        assert_eq!(app.current_item().map(|i| i.name.as_str()), Some("Severance"));
        assert!(app.current_item_revealed());
    }

    #[test]
    fn back_in_drilled_list_goes_up_a_level() {
        // Enter drills into the Series and moves focus to Content; Backspace
        // pops the drilled level and returns focus to the items pane.
        let mut app = app_with_item("Series");
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        assert!(app.is_drilled());
        assert_eq!(app.focus, Pane::Content);
        app.handle_key(press(KeyCode::Backspace));
        assert!(!app.is_drilled());
        assert_eq!(app.focus, Pane::LibraryItems);
    }

    #[test]
    fn drilling_renders_children_in_middle_pane_and_keeps_root_on_left() {
        // Drilling opens the children in the middle "Content" pane (titled
        // after the folder) while the left items pane stays on the library
        // root list.
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Discovery", "MusicAlbum")],
        }]);
        app.handle_key(press(KeyCode::Enter)); // drill into Discovery
        let _ = app.take_intents();
        app.fill_level(
            "id-Discovery",
            vec![typed_item("One More Time", "Audio"), typed_item("Aerodynamic", "Audio")],
        );
        let out = rendered(&app, 140, 30);
        // The left items pane still lists the album (root level).
        assert!(out.contains("Discovery"), "{out}");
        // The middle pane shows the folder's tracks.
        assert!(out.contains("One More Time"), "{out}");
        assert!(out.contains("Aerodynamic"), "{out}");
    }

    #[test]
    fn cursor_in_middle_pane_walks_drilled_items() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Album", "MusicAlbum")],
        }]);
        app.handle_key(press(KeyCode::Enter)); // drill, focus → Content
        let _ = app.take_intents();
        app.fill_level(
            "id-Album",
            vec![
                typed_item("Track A", "Audio"),
                typed_item("Track B", "Audio"),
                typed_item("Track C", "Audio"),
            ],
        );
        assert_eq!(app.focus, Pane::Content);
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.current_item().unwrap().name, "Track B");
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.current_item().unwrap().name, "Track C");
        // Enter on a drilled audio leaf queues a Play intent.
        app.handle_key(press(KeyCode::Enter));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::Play { media: MediaKind::Audio, item, .. }] if item.name == "Track C"
        ));
    }

    #[test]
    fn enter_on_root_album_while_drilled_swaps_the_middle_pane() {
        // Opening a different album from the root list should replace the
        // drilled level rather than push another one on top.
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Disc One", "MusicAlbum"), typed_item("Disc Two", "MusicAlbum")],
        }]);
        app.handle_key(press(KeyCode::Enter)); // drill into Disc One, focus → Content
        let _ = app.take_intents();
        assert_eq!(app.stack.len(), 2);
        // Walk back to the left items pane and pick the next album.
        app.focus = Pane::LibraryItems;
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        // Still exactly one drilled level deep, now Disc Two.
        assert_eq!(app.stack.len(), 2);
        assert_eq!(app.stack.last().unwrap().parent_id, "id-Disc Two");
        assert_eq!(app.focus, Pane::Content);
    }

    #[test]
    fn transport_keys_queue_intents() {
        let mut app = App::new();
        for code in [
            KeyCode::Char(' '),
            KeyCode::Char('s'),
            KeyCode::Char('+'),
            KeyCode::Char('-'),
            KeyCode::Char('>'),
            KeyCode::Char('<'),
        ] {
            app.handle_key(press(code));
        }
        assert_eq!(
            app.take_intents(),
            vec![
                Intent::TogglePause,
                Intent::Stop,
                Intent::VolumeUp,
                Intent::VolumeDown,
                Intent::SeekForward,
                Intent::SeekBackward,
            ]
        );
    }

    #[test]
    fn now_playing_bar_renders_when_active() {
        let mut app = App::new();
        app.now_playing = Some(NowPlaying {
            item_id: "trk1".to_string(),
            kind: MediaKind::Audio,
            title: "Some Song".to_string(),
            subtitle: Some("Some Artist".to_string()),
            position: Duration::from_secs(30),
            duration: Some(Duration::from_secs(200)),
            paused: false,
            volume: Some(80),
        });
        let out = rendered(&app, 100, 30);
        assert!(out.contains("Some Song"), "{out}");
        assert!(out.contains("Some Artist"));
        assert!(out.contains("vol 80%"));
        assert!(out.contains("0:30"));
    }


    #[test]
    fn t_opens_theme_picker_and_enter_emits_set_theme() {
        let mut app = App::new().with_available_themes(vec![
            "default".to_string(),
            "catppuccin-mocha".to_string(),
        ]);
        app.handle_key(press(KeyCode::Char('t')));
        // Picker opens on the active theme ("default" → index 0).
        let (names, selected) = app.theme_picker().expect("picker open");
        assert_eq!(names, ["default", "catppuccin-mocha"]);
        assert_eq!(selected, 0);

        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        assert!(app.theme_picker().is_none(), "Enter should close the picker");
        assert_eq!(
            app.take_intents().as_slice(),
            [Intent::SetTheme("catppuccin-mocha".into())]
        );
    }

    #[test]
    fn esc_closes_theme_picker_without_intent() {
        let mut app = App::new().with_available_themes(vec!["default".into()]);
        app.handle_key(press(KeyCode::Char('t')));
        assert!(app.theme_picker().is_some());
        app.handle_key(press(KeyCode::Esc));
        assert!(app.theme_picker().is_none());
        assert!(app.take_intents().is_empty());
    }

    #[test]
    fn theme_change_alters_rendered_border_color() {
        // Sanity check that themes actually feed into the renderer: the same UI
        // under two different themes should produce different border colors.
        fn border_fg(app: &App) -> ratatui::style::Color {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| render(frame, app, None)).unwrap();
            // x=0, y=0 is the top-left of the sidebar border (which the sidebar
            // is focused on, so it picks up `focused_border`).
            terminal.backend().buffer().cell((0, 0)).unwrap().fg
        }
        let mut app = App::new();
        let default_fg = border_fg(&app);
        app.set_theme(crate::theme::load("catppuccin-latte").unwrap());
        let latte_fg = border_fg(&app);
        assert_ne!(
            default_fg, latte_fg,
            "switching themes should change the rendered border color"
        );
    }

    #[test]
    fn set_theme_changes_active_theme_name() {
        let mut app = App::new();
        assert_eq!(app.theme.name(), "default");
        app.set_theme(crate::theme::load("catppuccin-latte").unwrap());
        assert_eq!(app.theme.name(), "catppuccin-latte");
    }

    #[test]
    fn now_playing_bar_shows_idle_placeholder_when_nothing_plays() {
        // The bar is always present (fixed layout), showing an idle hint.
        let out = rendered(&App::new(), 100, 30);
        assert!(out.contains("Nothing playing"), "{out}");
    }

    #[test]
    fn renders_panes_and_status_bar() {
        let out = rendered(&App::new(), 120, 40);
        assert!(out.contains("Content")); // middle pane title
        assert!(out.contains("Sections")); // left bottom pane title
        assert!(out.contains("Movies")); // top-bar library chip + items breadcrumb
        assert!(out.contains("The Matrix")); // a list item from the selected library
        assert!(out.contains("F1 help"));
    }

    #[test]
    fn middle_pane_is_placeholder_until_drilled() {
        // Middle pane is reserved for an album/playlist/series' contents and
        // stays a placeholder until the user drills into a folder. The focused
        // item's metadata renders in the info pane (right column top) instead.
        let app = App::with_libraries(vec![Library {
            id: "movies".to_string(),
            name: "Movies".to_string(),
            collection_type: Some("movies".to_string()),
            items: vec![Item {
                id: "1".to_string(),
                name: "The Matrix".to_string(),
                overview: Some("Neo learns the truth.".to_string()),
                production_year: Some(1999),
                run_time_ticks: Some(136 * 60 * 10_000_000),
                kind: Some("Movie".to_string()),
                primary_image_tag: None,
                is_folder: false,
                is_favorite: false,
                is_played: false,
                normalization_gain_db: None,
                album_id: None,
                album_name: None,
                primary_artist_id: None,
                primary_artist_name: None,
            }],
        }]);
        let out = rendered(&app, 120, 30);
        assert!(out.contains("Select an album"), "{out}");
        assert!(!out.contains("Neo learns the truth"), "{out}");
    }

    #[test]
    fn selection_change_hides_revealed_details() {
        let mut app = App::with_libraries(vec![Library {
            id: "movies".to_string(),
            name: "Movies".to_string(),
            collection_type: Some("movies".to_string()),
            items: vec![
                Item { id: "1".to_string(), name: "A".to_string(), ..Default::default() },
                Item { id: "2".to_string(), name: "B".to_string(), ..Default::default() },
            ],
        }]);
        app.reveal_item("1".to_string());
        assert!(app.current_item_revealed());
        // clear_current_detail mirrors what the run loop does on selection move.
        app.clear_current_detail();
        assert!(!app.current_item_revealed());
    }

    #[test]
    fn help_overlay_renders_grouped_bindings() {
        let mut app = App::new();
        app.show_help = true;
        // The cheatsheet has grown enough (Navigation/Playback/Queue/Library/
        // General + the Top-bar built-ins) that a 30-row buffer clips the
        // tail. Bump high enough to fit every group.
        let out = rendered(&app, 100, 60);
        assert!(out.contains("Keybindings"));
        assert!(out.contains("Navigation"));
        assert!(out.contains("Queue"));
        assert!(out.contains("Quit"));
    }

    #[test]
    fn help_overlay_lists_queue_bindings() {
        let mut app = App::new();
        app.show_help = true;
        let out = rendered(&app, 100, 60);
        // Every action surfaces both its key glyph and its description so the
        // user can find shuffle/repeat/next/prev at a glance.
        assert!(out.contains("Next track"), "{out}");
        assert!(out.contains("Previous track"));
        assert!(out.contains("Toggle shuffle"));
        assert!(out.contains("Cycle repeat mode"));
    }

    #[test]
    fn error_modal_captures_input_until_dismissed() {
        let mut app = App::new();
        app.show_error("network unreachable");
        assert!(app.error.is_some());
        // Navigation keys are swallowed while the modal is open.
        app.handle_key(press(KeyCode::Down));
        assert!(app.error.is_some());
        assert_eq!(app.library_selected, 0);
        // Enter dismisses it.
        app.handle_key(press(KeyCode::Enter));
        assert!(app.error.is_none());
    }

    #[test]
    fn error_modal_copy_sets_copied_flag() {
        let mut app = App::new();
        app.show_error("boom");
        app.handle_key(press(KeyCode::Char('y')));
        assert!(app.error_copied);
    }

    #[test]
    fn renders_error_modal() {
        let mut app = App::new();
        app.show_error("could not reach server");
        let out = rendered(&app, 100, 30);
        assert!(out.contains("Something went wrong"));
        assert!(out.contains("could not reach server"));
        assert!(out.contains("Log:"));
    }

    #[test]
    fn sections_for_returns_kind_specific_lists() {
        let music = sections_for(Some("music"));
        let movies = sections_for(Some("movies"));
        assert!(music.iter().any(|s| s.name == "Albums"));
        assert!(music.iter().any(|s| s.name == "Album Artists"));
        assert!(movies.iter().any(|s| s.name == "Collections"));
        // The first section is always "All".
        assert_eq!(music[0].name, "All");
    }

    #[test]
    fn arrows_move_section_selection_when_focused_on_sections() {
        let mut app = App::new();
        // Switch to the music library (3rd in the demo set) so sections exist.
        app.handle_key(press(KeyCode::Char('3')));
        app.focus = Pane::LibrarySections;
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.section_selected, 1);
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.section_selected, 2);
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.section_selected, 1);
    }

    #[test]
    fn enter_on_section_queues_apply_section_intent() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('3'))); // music library
        // Drop the SaveLastLibrary intent the library switch just queued so
        // the rest of the assertions focus on the section flow.
        let _ = app.take_intents();
        app.focus = Pane::LibrarySections;
        app.handle_key(press(KeyCode::Down)); // Albums
        app.handle_key(press(KeyCode::Enter));
        let intents = app.take_intents();
        // apply_section emits the ApplySection fetch + a SaveSectionMemory
        // intent so the user's choice persists.
        assert_eq!(intents.len(), 2);
        match &intents[0] {
            Intent::ApplySection { library_id, section, .. } => {
                assert_eq!(library_id, "music");
                assert_eq!(section.name, "Albums");
            }
            other => panic!("expected ApplySection, got {other:?}"),
        }
        match &intents[1] {
            Intent::SaveSectionMemory(memory) => {
                assert_eq!(memory.get("music").map(String::as_str), Some("Albums"));
            }
            other => panic!("expected SaveSectionMemory, got {other:?}"),
        }
        // The root level is marked loading until the fetch returns.
        assert!(app.current_level().unwrap().loading);
    }

    #[test]
    fn go_to_album_drills_into_parent_album() {
        let track = Item {
            id: "track-1".to_string(),
            name: "Get Lucky".to_string(),
            kind: Some("Audio".to_string()),
            album_id: Some("album-1".to_string()),
            album_name: Some("Random Access Memories".to_string()),
            ..Item::default()
        };
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![track],
        }]);
        let entries = app.item_menu_entries();
        assert!(entries.iter().any(|(label, action)| {
            label == "Go to album: Random Access Memories"
                && *action == ItemMenuAction::GoToAlbum
        }));
        app.go_to_album();
        let level = app.current_level().unwrap();
        assert_eq!(level.parent_id, "album-1");
        assert_eq!(level.title, "Random Access Memories");
    }

    #[test]
    fn go_to_artist_drills_into_primary_artist() {
        let track = Item {
            id: "track-1".to_string(),
            name: "Get Lucky".to_string(),
            kind: Some("Audio".to_string()),
            primary_artist_id: Some("artist-1".to_string()),
            primary_artist_name: Some("Daft Punk".to_string()),
            ..Item::default()
        };
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![track],
        }]);
        let entries = app.item_menu_entries();
        assert!(entries.iter().any(|(label, action)| {
            label == "Go to artist: Daft Punk" && *action == ItemMenuAction::GoToArtist
        }));
        app.go_to_artist();
        let level = app.current_level().unwrap();
        assert_eq!(level.parent_id, "artist-1");
        assert_eq!(level.title, "Daft Punk");
    }

    #[test]
    fn go_to_actions_hidden_when_no_artist_or_album() {
        let movie = Item {
            id: "1".to_string(),
            name: "The Matrix".to_string(),
            kind: Some("Movie".to_string()),
            ..Item::default()
        };
        let app = App::with_libraries(vec![Library {
            id: "movies".to_string(),
            name: "Movies".to_string(),
            collection_type: Some("movies".to_string()),
            items: vec![movie],
        }]);
        let entries = app.item_menu_entries();
        assert!(!entries
            .iter()
            .any(|(_, a)| matches!(a, ItemMenuAction::GoToAlbum | ItemMenuAction::GoToArtist)));
    }

    #[test]
    fn search_scopes_to_current_section_item_types() {
        let mut app = App::new();
        // Switch to the music library and pick the "Albums" section so any
        // subsequent search is scoped to MusicAlbum.
        app.handle_key(press(KeyCode::Char('3')));
        app.focus = Pane::LibrarySections;
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        app.focus = Pane::LibraryItems;

        app.handle_key(press(KeyCode::Char('/')));
        for c in "discovery".chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
        app.handle_key(press(KeyCode::Enter));

        let intents = app.take_intents();
        let search = intents
            .iter()
            .find_map(|i| match i {
                Intent::Search {
                    query,
                    item_types,
                    scope_label,
                } => Some((query.clone(), item_types.clone(), scope_label.clone())),
                _ => None,
            })
            .expect("expected a Search intent");
        assert_eq!(search.0, "discovery");
        assert_eq!(search.1, vec!["MusicAlbum".to_string()]);
        assert_eq!(search.2, "Albums");
        assert_eq!(
            app.current_level().unwrap().title,
            "Search · Albums: discovery"
        );
    }

    #[test]
    fn search_on_section_all_has_no_item_types() {
        let mut app = App::new();
        // Music library, default section is "All" (item_types empty).
        app.handle_key(press(KeyCode::Char('3')));
        let _ = app.take_intents();

        app.handle_key(press(KeyCode::Char('/')));
        for c in "x".chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
        app.handle_key(press(KeyCode::Enter));

        let intents = app.take_intents();
        let search = intents
            .iter()
            .find_map(|i| match i {
                Intent::Search {
                    item_types,
                    scope_label,
                    ..
                } => Some((item_types.clone(), scope_label.clone())),
                _ => None,
            })
            .expect("expected a Search intent");
        assert!(search.0.is_empty());
        assert!(search.1.is_empty());
        assert_eq!(app.current_level().unwrap().title, "Search: x");
    }

    #[test]
    fn slash_opens_search_input_and_chars_build_the_query() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('/')));
        assert_eq!(app.search_query.as_deref(), Some(""));
        assert_eq!(app.focus, Pane::TopBar);
        for c in "foo".chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
        assert_eq!(app.search_query.as_deref(), Some("foo"));
    }

    #[test]
    fn search_input_backspace_and_esc() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('/')));
        for c in "foo".chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
        app.handle_key(press(KeyCode::Backspace));
        assert_eq!(app.search_query.as_deref(), Some("fo"));
        app.handle_key(press(KeyCode::Esc));
        assert!(app.search_query.is_none());
        assert_eq!(app.focus, Pane::LibraryItems);
    }

    #[test]
    fn enter_in_search_fires_search_intent_with_query() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('/')));
        for c in "matrix".chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
        app.handle_key(press(KeyCode::Enter));
        let intents = app.take_intents();
        // Submitting a search remembers the query (SaveSearchHistory) then
        // kicks off the fetch (Search).
        assert_eq!(intents.len(), 2);
        match &intents[0] {
            Intent::SaveSearchHistory(history) => {
                assert_eq!(history.as_slice(), &["matrix".to_string()]);
            }
            other => panic!("expected SaveSearchHistory, got {other:?}"),
        }
        match &intents[1] {
            Intent::Search {
                query,
                item_types,
                scope_label,
            } => {
                assert_eq!(query, "matrix");
                // Default library has no collection type → only section
                // available is "All" with empty item_types.
                assert!(item_types.is_empty());
                assert!(scope_label.is_empty());
            }
            other => panic!("expected Search, got {other:?}"),
        }
        assert!(app.current_level().unwrap().loading);
        assert_eq!(app.current_level().unwrap().title, "Search: matrix");
    }

    #[test]
    fn apply_root_items_fills_the_loading_root_level() {
        let mut app = App::new();
        // Trigger ApplySection so stack[0] becomes a loading root level.
        app.handle_key(press(KeyCode::Char('3'))); // music
        app.focus = Pane::LibrarySections;
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        assert!(app.current_level().unwrap().loading);
        app.apply_root_items(
            "music",
            "Music · Albums".to_string(),
            vec![Item::demo("Discovery"), Item::demo("In Rainbows")],
        );
        let level = app.current_level().unwrap();
        assert!(!level.loading);
        assert_eq!(level.items.len(), 2);
        assert_eq!(level.title, "Music · Albums");
    }

    #[test]
    fn build_queue_collects_audio_siblings_and_indexes_starter() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![
                typed_item("Track A", "Audio"),
                typed_item("Folder", "MusicAlbum"),
                typed_item("Track B", "Audio"),
                typed_item("Track C", "Audio"),
            ],
        }]);
        let started = app.current_level().unwrap().items[2].clone(); // Track B
        app.build_queue_for(&started);
        // Audio-only items (skipping the folder), starter at index 1.
        assert_eq!(app.queue.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), vec!["Track A", "Track B", "Track C"]);
        assert_eq!(app.queue_index, Some(1));
        assert_eq!(app.current_queue_track().unwrap().name, "Track B");
        assert_eq!(
            app.upcoming_queue().iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            vec!["Track C"],
        );
    }

    #[test]
    fn advance_queue_walks_to_the_end() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("A", "Audio"), typed_item("B", "Audio")],
        }]);
        let starter = app.current_level().unwrap().items[0].clone();
        app.build_queue_for(&starter);
        let next = app.advance_queue().expect("one more track to play");
        assert_eq!(next.name, "B");
        assert_eq!(app.queue_index, Some(1));
        // No further tracks; second advance returns None and leaves the index
        // pinned at the last track.
        assert!(app.advance_queue().is_none());
        assert_eq!(app.queue_index, Some(1));
    }

    #[test]
    fn repeat_mode_cycles_in_order() {
        assert_eq!(RepeatMode::Off.cycle(), RepeatMode::All);
        assert_eq!(RepeatMode::All.cycle(), RepeatMode::One);
        assert_eq!(RepeatMode::One.cycle(), RepeatMode::Off);
    }

    #[test]
    fn r_key_cycles_repeat_mode_and_flashes_status() {
        let mut app = App::new();
        assert_eq!(app.repeat_mode, RepeatMode::Off);
        app.handle_key(press(KeyCode::Char('r')));
        assert_eq!(app.repeat_mode, RepeatMode::All);
        app.handle_key(press(KeyCode::Char('r')));
        assert_eq!(app.repeat_mode, RepeatMode::One);
        app.handle_key(press(KeyCode::Char('r')));
        assert_eq!(app.repeat_mode, RepeatMode::Off);
    }

    #[test]
    fn z_key_toggles_shuffle() {
        let mut app = App::new();
        assert!(!app.shuffle);
        app.handle_key(press(KeyCode::Char('z')));
        assert!(app.shuffle);
        app.handle_key(press(KeyCode::Char('z')));
        assert!(!app.shuffle);
    }

    #[test]
    fn shuffle_and_repeat_emit_save_audio_prefs_intents() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('r'))); // → Repeat::All
        app.handle_key(press(KeyCode::Char('z'))); // → shuffle on
        let intents = app.take_intents();
        assert!(matches!(
            intents.as_slice(),
            [
                Intent::SaveAudioPrefs { repeat_mode: RepeatMode::All, shuffle: false },
                Intent::SaveAudioPrefs { repeat_mode: RepeatMode::All, shuffle: true },
            ],
        ));
    }

    #[test]
    fn with_audio_prefs_seeds_runtime_state() {
        let app = App::new().with_audio_prefs(RepeatMode::One, true);
        assert_eq!(app.repeat_mode, RepeatMode::One);
        assert!(app.shuffle);
    }

    #[test]
    fn digit_switch_emits_save_last_library_intent() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('2')));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::SaveLastLibrary(id)] if id == "tv"
        ));
    }

    #[test]
    fn with_last_library_restores_focused_library() {
        let app = App::new().with_last_library(Some("music".to_string()));
        assert_eq!(app.library_selected, 2);
        // Unknown id keeps the default selection rather than blowing up.
        let app = App::new().with_last_library(Some("nonexistent".to_string()));
        assert_eq!(app.library_selected, 0);
    }

    #[test]
    fn search_history_cycles_with_up_and_down() {
        let mut app = App::new().with_search_history(vec![
            "matrix".to_string(),
            "neo".to_string(),
            "trinity".to_string(),
        ]);
        app.handle_key(press(KeyCode::Char('/')));
        // Up walks toward older entries.
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.search_query.as_deref(), Some("matrix"));
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.search_query.as_deref(), Some("neo"));
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.search_query.as_deref(), Some("trinity"));
        // Past the end of the list clamps.
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.search_query.as_deref(), Some("trinity"));
        // Down walks back toward the live query.
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.search_query.as_deref(), Some("neo"));
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.search_query.as_deref(), Some("matrix"));
        app.handle_key(press(KeyCode::Down));
        // Past the newest entry, back to a blank input.
        assert_eq!(app.search_query.as_deref(), Some(""));
    }

    #[test]
    fn submitting_search_dedupes_and_caps_history() {
        let mut app = App::new().with_search_history(vec!["foo".to_string()]);
        app.handle_key(press(KeyCode::Char('/')));
        for c in "foo".chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
        app.handle_key(press(KeyCode::Enter));
        // "foo" was already in history; resubmitting moves it to the front
        // without duplicating.
        assert_eq!(app.search_history, vec!["foo".to_string()]);
    }

    #[test]
    fn with_section_memory_restores_selected_section_on_startup() {
        // Pretend disk had "Music → Albums" saved from a prior session.
        let mut memory = std::collections::HashMap::new();
        memory.insert("music".to_string(), "Albums".to_string());
        let mut app = App::new().with_section_memory(memory);
        // Start on Music so the section_selected reflects the restored value.
        app.handle_key(press(KeyCode::Char('3')));
        assert_eq!(app.section_selected, 1); // Albums is index 1 for music
    }

    #[test]
    fn section_memory_persists_across_library_switches() {
        let mut app = App::new();
        // Switch to Music (index 2), apply the "Albums" section (index 1).
        app.handle_key(press(KeyCode::Char('3')));
        app.focus = Pane::LibrarySections;
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        // Hop to Movies and back; the music library should remember Albums.
        app.handle_key(press(KeyCode::Char('1')));
        assert_eq!(app.section_selected, 0);
        app.handle_key(press(KeyCode::Char('3')));
        assert_eq!(app.section_selected, 1);
    }

    #[test]
    fn repeat_mode_pref_round_trips() {
        for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            let pref: crate::config::RepeatModePref = mode.into();
            let back: RepeatMode = pref.into();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn info_view_lists_artist_albums_and_appears_on_with_liked_marker() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![Item {
                id: "artist-1".to_string(),
                name: "Daft Punk".to_string(),
                kind: Some("MusicArtist".to_string()),
                is_folder: true,
                ..Default::default()
            }],
        }]);
        app.reveal_item("artist-1".to_string());
        app.set_context_view(ContextTopView::Info);
        app.set_current_detail(
            "artist-1",
            ItemDetail {
                overview: Some("French electronic duo.".to_string()),
                artist_albums: vec![
                    Item {
                        id: "alb-1".to_string(),
                        name: "Discovery".to_string(),
                        is_favorite: true,
                        ..Default::default()
                    },
                    Item {
                        id: "alb-2".to_string(),
                        name: "Random Access Memories".to_string(),
                        ..Default::default()
                    },
                ],
                appears_on: vec![Item {
                    id: "alb-3".to_string(),
                    name: "Tron: Legacy OST".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        let out = rendered(&app, 140, 40);
        assert!(out.contains("French electronic duo"), "{out}");
        assert!(out.contains("Albums"), "{out}");
        assert!(out.contains("Appears on"), "{out}");
        assert!(out.contains("Discovery"), "{out}");
        assert!(out.contains("Tron: Legacy OST"), "{out}");
        // Liked album shows a heart marker.
        assert!(out.contains("♥ Discovery"), "{out}");
    }

    #[test]
    fn info_pane_renders_scrollbar_when_content_overflows() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![Item {
                id: "artist-1".to_string(),
                name: "Prolific Artist".to_string(),
                kind: Some("MusicArtist".to_string()),
                is_folder: true,
                ..Default::default()
            }],
        }]);
        app.reveal_item("artist-1".to_string());
        app.set_context_view(ContextTopView::Info);
        let many_albums: Vec<Item> = (0..50)
            .map(|i| Item {
                id: format!("alb-{i}"),
                name: format!("Album {i}"),
                ..Default::default()
            })
            .collect();
        app.set_current_detail(
            "artist-1",
            ItemDetail {
                overview: Some("Long discography.".to_string()),
                artist_albums: many_albums,
                ..Default::default()
            },
        );
        let out = rendered(&app, 140, 24);
        // Ratatui's default vertical scrollbar uses unicode block glyphs for
        // the thumb and track; either appearing in the buffer proves it drew.
        let has_glyph = out.contains('█') || out.contains('░') || out.contains('▐');
        assert!(has_glyph, "expected scrollbar glyph, got:\n{out}");
    }

    #[test]
    fn show_info_menu_entry_switches_view_and_emits_intent() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Track A", "Audio")],
        }]);
        app.handle_key(press(KeyCode::Char('p')));
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        // Audio always exposes both view entries so lyrics can be loaded even
        // when the pane defaults to the lyrics view.
        assert_eq!(entries[0], "Show lyrics");
        assert_eq!(entries[1], "Show info");
        // Pick "Show info" (second entry).
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        assert_eq!(app.context_view(), ContextTopView::Info);
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::LoadCurrentDetail { .. }]
        ));
    }

    #[test]
    fn show_lyrics_menu_entry_switches_view_and_fetches() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Track A", "Audio")],
        }]);
        // Flip to info view first, then back to lyrics via the popup.
        app.set_context_view(ContextTopView::Info);
        app.handle_key(press(KeyCode::Char('p')));
        // First entry is "Show lyrics" — Enter selects it.
        app.handle_key(press(KeyCode::Enter));
        assert_eq!(app.context_view(), ContextTopView::Lyrics);
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::LoadCurrentDetail { .. }]
        ));
    }

    #[test]
    fn p_opens_item_menu_and_load_info_emits_intent() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Char('p')));
        // Menu is open with "Load info" highlighted first.
        let (title, entries, sel) = app.popup_menu().expect("item menu open");
        assert_eq!(title, "Actions");
        assert_eq!(sel, 0);
        assert_eq!(entries[0], "Load info");
        // Enter on "Load info" closes the menu and queues a detail fetch.
        app.handle_key(press(KeyCode::Enter));
        assert!(app.popup_menu().is_none());
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::LoadCurrentDetail { item_id, .. }] if item_id == "id-Thing"
        ));
    }

    #[test]
    fn shift_p_opens_client_menu_and_quit_entry_quits() {
        let mut app = App::new();
        // Shift+p arrives as Char('P') + SHIFT modifier.
        app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
        let (title, entries, _) = app.popup_menu().expect("client menu open");
        assert_eq!(title, "Client settings");
        assert!(entries.iter().any(|e| e == "Theme…"));
        // Walk to the "Quit" entry (last) and activate.
        for _ in 0..entries.len() - 1 {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        assert!(app.popup_menu().is_none());
        assert!(app.should_quit);
    }

    #[test]
    fn client_menu_includes_sync_and_visible_libraries() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
        let (_, entries, _) = app.popup_menu().expect("client menu open");
        assert!(entries.iter().any(|e| e == "Sync with server now"));
        assert!(entries.iter().any(|e| e == "Visible libraries…"));
    }

    #[test]
    fn sync_now_entry_emits_sync_intent() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
        // Client menu: Theme, Sync, Visible, Quit. "Sync" is index 1.
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::SyncLibraries]
        ));
    }

    #[test]
    fn visible_libraries_entry_opens_picker_and_emits_load_intent() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
        for _ in 0..2 {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        assert!(app.library_picker().is_some());
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::LoadAllLibraryMeta]
        ));
    }

    #[test]
    fn library_picker_save_emits_visible_libraries_intent() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
        for _ in 0..2 {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        app.set_library_picker_entries(vec![
            ("lib-a".to_string(), "Movies".to_string(), true),
            ("lib-b".to_string(), "Music".to_string(), true),
        ]);
        // Space toggles entry 0 off.
        app.handle_key(press(KeyCode::Char(' ')));
        app.handle_key(press(KeyCode::Enter));
        assert!(app.library_picker().is_none());
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::SaveVisibleLibraries(ids)] if ids == &vec!["lib-b".to_string()]
        ));
    }

    #[test]
    fn item_menu_includes_audio_specific_actions() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Track A", "Audio")],
        }]);
        app.handle_key(press(KeyCode::Char('p')));
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        assert!(entries.iter().any(|e| e == "Add to playlist…"));
        assert!(entries.iter().any(|e| e == "Instant mix"));
        assert!(entries.iter().any(|e| e == "Dislike track"));
        assert!(entries.iter().any(|e| e == "Copy URL to clipboard"));
    }

    #[test]
    fn item_menu_hides_audio_actions_for_video_items() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Char('p')));
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        // Instant mix + Dislike are audio-only; the rest of the menu is universal.
        assert!(!entries.iter().any(|e| e == "Instant mix"));
        assert!(!entries.iter().any(|e| e == "Dislike track"));
        assert!(entries.iter().any(|e| e == "Add to playlist…"));
        assert!(entries.iter().any(|e| e == "Copy URL to clipboard"));
    }

    #[test]
    fn instant_mix_entry_emits_intent() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Track A", "Audio")],
        }]);
        app.handle_key(press(KeyCode::Char('p')));
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        let idx = entries
            .iter()
            .position(|e| e == "Instant mix")
            .expect("Instant mix entry present");
        for _ in 0..idx {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::InstantMix { item }] if item.name == "Track A"
        ));
    }

    #[test]
    fn dislike_entry_emits_intent() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Track A", "Audio")],
        }]);
        app.handle_key(press(KeyCode::Char('p')));
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        let idx = entries
            .iter()
            .position(|e| e == "Dislike track")
            .expect("Dislike entry present");
        for _ in 0..idx {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::Dislike { item_id }] if item_id == "id-Track A"
        ));
    }

    #[test]
    fn copy_url_entry_emits_intent() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Char('p')));
        // Movie order: Load info, Play, Add to favorites, Copy URL.
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        let copy_index = entries
            .iter()
            .position(|e| e == "Copy URL to clipboard")
            .expect("Copy URL entry present");
        for _ in 0..copy_index {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::CopyItemUrl { item_id, .. }] if item_id == "id-Thing"
        ));
    }

    #[test]
    fn add_to_playlist_entry_opens_picker_and_enter_emits_intent() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("Track A", "Audio")],
        }]);
        app.handle_key(press(KeyCode::Char('p')));
        // Walk to "Add to playlist…" (index 5: lyrics, info, play, favorite, mark played, playlist).
        for _ in 0..5 {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        assert!(app.playlist_picker().is_some());
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::LoadPlaylists { target_item_id }] if target_item_id == "id-Track A"
        ));
        app.set_playlist_picker_entries(vec![("pl-1".to_string(), "Drive Mix".to_string())]);
        app.handle_key(press(KeyCode::Enter));
        assert!(app.playlist_picker().is_none());
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::AddToPlaylist { playlist_id, item_id }]
                if playlist_id == "pl-1" && item_id == "id-Track A"
        ));
    }

    #[test]
    fn video_item_menu_includes_track_entries() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Char('p')));
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        assert!(entries.iter().any(|e| e.starts_with("Audio track:")));
        assert!(entries.iter().any(|e| e.starts_with("Subtitles:")));
    }

    #[test]
    fn audio_track_entry_opens_picker_and_enter_records_choice() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Char('p')));
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        let idx = entries
            .iter()
            .position(|e| e.starts_with("Audio track:"))
            .expect("Audio track entry");
        for _ in 0..idx {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        assert!(app.video_track_picker().is_some());
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::LoadVideoTracks { item_id, kind }]
                if item_id == "id-Thing" && *kind == VideoTrackKind::Audio
        ));
        app.set_video_track_picker_entries(vec![
            TrackPickerEntry {
                label: "Auto".to_string(),
                choice: TrackChoice::Auto,
            },
            TrackPickerEntry {
                label: "#1 English".to_string(),
                choice: TrackChoice::Pick(1),
            },
        ]);
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        assert!(app.video_track_picker().is_none());
        let options = app.video_options_for("id-Thing");
        assert!(matches!(options.audio, Some(TrackChoice::Pick(1))));
    }

    #[test]
    fn subtitle_picker_off_sets_off_choice() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Char('p')));
        let (_, entries, _) = app.popup_menu().expect("item menu open");
        let idx = entries
            .iter()
            .position(|e| e.starts_with("Subtitles:"))
            .expect("Subtitles entry");
        for _ in 0..idx {
            app.handle_key(press(KeyCode::Down));
        }
        app.handle_key(press(KeyCode::Enter));
        let _ = app.take_intents();
        app.set_video_track_picker_entries(vec![
            TrackPickerEntry {
                label: "Auto".to_string(),
                choice: TrackChoice::Auto,
            },
            TrackPickerEntry {
                label: "Off".to_string(),
                choice: TrackChoice::Off,
            },
        ]);
        app.handle_key(press(KeyCode::Down));
        app.handle_key(press(KeyCode::Enter));
        let options = app.video_options_for("id-Thing");
        assert!(matches!(options.subtitle, Some(TrackChoice::Off)));
    }

    #[test]
    fn go_to_prefix_then_target_changes_focus() {
        let mut app = App::new();
        // g + i → Items pane
        app.handle_key(press(KeyCode::Char('g')));
        assert!(app.awaiting_go_to());
        app.handle_key(press(KeyCode::Char('i')));
        assert!(!app.awaiting_go_to());
        assert_eq!(app.focus, Pane::LibraryItems);
        // g + t → Top bar (libraries)
        app.handle_key(press(KeyCode::Char('g')));
        app.handle_key(press(KeyCode::Char('t')));
        assert_eq!(app.focus, Pane::TopBar);
        // g + q → Context bottom (queue)
        app.handle_key(press(KeyCode::Char('g')));
        app.handle_key(press(KeyCode::Char('q')));
        assert_eq!(app.focus, Pane::ContextBottom);
    }

    #[test]
    fn go_to_esc_cancels_chord_without_focus_change() {
        let mut app = App::new();
        let before = app.focus;
        app.handle_key(press(KeyCode::Char('g')));
        assert!(app.awaiting_go_to());
        app.handle_key(press(KeyCode::Esc));
        assert!(!app.awaiting_go_to());
        assert_eq!(app.focus, before);
    }

    #[test]
    fn go_to_s_opens_global_search() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('g')));
        app.handle_key(press(KeyCode::Char('s')));
        assert_eq!(app.focus, Pane::GlobalSearch);
        assert!(app.global_search_state().input_focused);
    }

    #[test]
    fn slash_opens_library_scoped_search() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('/')));
        assert_eq!(app.focus, Pane::TopBar);
        assert!(app.search_query.is_some());
    }

    #[test]
    fn global_search_input_typing_appends_query() {
        let mut app = App::new();
        app.enter_global_search();
        app.handle_key(press(KeyCode::Char('m')));
        app.handle_key(press(KeyCode::Char('x')));
        assert_eq!(app.global_search_state().query, "mx");
        app.handle_key(press(KeyCode::Backspace));
        assert_eq!(app.global_search_state().query, "m");
    }

    #[test]
    fn global_search_enter_emits_global_search_intent() {
        let mut app = App::new();
        app.enter_global_search();
        app.handle_key(press(KeyCode::Char('a')));
        app.handle_key(press(KeyCode::Enter));
        let intents = app.take_intents();
        assert!(intents.iter().any(|i| matches!(
            i,
            Intent::GlobalSearch { query } if query == "a"
        )));
    }

    #[test]
    fn go_to_works_from_inside_media_options_view() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Enter));
        assert!(app.media_options_view().is_some());
        assert_eq!(app.focus, Pane::Content);
        app.handle_key(press(KeyCode::Char('g')));
        assert!(app.awaiting_go_to());
        app.handle_key(press(KeyCode::Char('t')));
        assert!(!app.awaiting_go_to());
        assert!(app.media_options_view().is_none());
        assert_eq!(app.focus, Pane::TopBar);
    }

    #[test]
    fn go_to_works_from_inside_popup_menu() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
        assert!(app.popup_menu().is_some());
        app.handle_key(press(KeyCode::Char('g')));
        app.handle_key(press(KeyCode::Char('i')));
        assert!(app.popup_menu().is_none());
        assert_eq!(app.focus, Pane::LibraryItems);
    }

    #[test]
    fn go_to_chips_render_in_status_bar() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('g')));
        let out = rendered(&app, 140, 24);
        assert!(out.contains("Go to:"), "{out}");
        assert!(out.contains("Esc cancel"), "{out}");
    }

    #[test]
    fn esc_closes_popup_without_action() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Char('p')));
        assert!(app.popup_menu().is_some());
        app.handle_key(press(KeyCode::Esc));
        assert!(app.popup_menu().is_none());
        assert!(app.take_intents().is_empty());
    }

    #[test]
    fn queue_nav_keys_emit_intents() {
        // `p` is reserved for the item-options popup; `b` walks back.
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('n')));
        app.handle_key(press(KeyCode::Char('b')));
        assert_eq!(
            app.take_intents(),
            vec![Intent::QueueNext, Intent::QueuePrev]
        );
    }

    #[test]
    fn advance_queue_with_repeat_all_wraps_to_start() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("A", "Audio"), typed_item("B", "Audio")],
        }]);
        let start = app.current_level().unwrap().items[1].clone(); // B (index 1)
        app.build_queue_for(&start);
        app.repeat_mode = RepeatMode::All;
        // Past the end wraps back to track A.
        assert_eq!(app.advance_queue().unwrap().name, "A");
        assert_eq!(app.queue_index, Some(0));
    }

    #[test]
    fn advance_queue_with_repeat_one_replays_current() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("A", "Audio"), typed_item("B", "Audio")],
        }]);
        let start = app.current_level().unwrap().items[0].clone();
        app.build_queue_for(&start);
        app.repeat_mode = RepeatMode::One;
        assert_eq!(app.advance_queue().unwrap().name, "A");
        assert_eq!(app.queue_index, Some(0));
    }

    #[test]
    fn previous_in_queue_steps_back_or_returns_none_at_start() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("A", "Audio"), typed_item("B", "Audio")],
        }]);
        let start = app.current_level().unwrap().items[1].clone();
        app.build_queue_for(&start);
        assert_eq!(app.previous_in_queue().unwrap().name, "A");
        // Already at start; without repeat there's nowhere to go.
        assert!(app.previous_in_queue().is_none());
    }

    #[test]
    fn toggle_shuffle_pins_current_track_at_index_0() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![
                typed_item("A", "Audio"),
                typed_item("B", "Audio"),
                typed_item("C", "Audio"),
            ],
        }]);
        let start = app.current_level().unwrap().items[1].clone(); // B
        app.build_queue_for(&start);
        app.toggle_shuffle();
        assert!(app.shuffle);
        assert_eq!(app.queue_index, Some(0));
        // The pinned current track is still B regardless of how the rest got
        // shuffled.
        assert_eq!(app.current_queue_track().unwrap().name, "B");
    }

    #[test]
    fn clear_queue_resets_index() {
        let mut app = App::with_libraries(vec![Library {
            id: "music".to_string(),
            name: "Music".to_string(),
            collection_type: Some("music".to_string()),
            items: vec![typed_item("A", "Audio")],
        }]);
        app.build_queue_for(&app.current_level().unwrap().items[0].clone());
        app.clear_queue();
        assert!(app.queue.is_empty());
        assert!(app.queue_index.is_none());
        assert!(app.current_queue_track().is_none());
    }

    #[test]
    fn set_current_detail_only_applies_when_id_matches_selection() {
        let mut app = app_with_item("Movie");
        let detail = ItemDetail {
            cast: vec![Person {
                id: None,
                name: "Neo".to_string(),
                role: Some("Hero".to_string()),
                kind: Some("Actor".to_string()),
            }],
            genres: vec!["Sci-Fi".to_string()],
            ..Default::default()
        };
        // Matching id is accepted.
        app.set_current_detail("id-Thing", detail.clone());
        assert!(app.current_detail().is_some());
        assert_eq!(app.current_detail().unwrap().cast[0].name, "Neo");
        // Stale id from a past selection is ignored.
        app.clear_current_detail();
        app.set_current_detail("id-Stale", detail);
        assert!(app.current_detail().is_none());
    }

    #[test]
    fn cheatsheet_includes_top_bar_built_ins() {
        let mut app = App::new();
        app.show_help = true;
        let out = rendered(&app, 120, 60);
        assert!(out.contains("Top bar"), "{out}");
        assert!(out.contains("1 – 9"), "{out}");
        assert!(out.contains("Switch library"));
        assert!(out.contains("Open search"));
    }

    #[test]
    fn digit_keys_are_ignored_inside_search_input() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('/')));
        app.handle_key(press(KeyCode::Char('2')));
        // '2' is part of the query, not a library switch.
        assert_eq!(app.library_selected, 0);
        assert_eq!(app.search_query.as_deref(), Some("2"));
    }
}
