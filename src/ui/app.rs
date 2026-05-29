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
use super::{cheatsheet, error_modal, layout, panes, theme_picker};
use crate::theme::Theme;

pub(crate) type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Which pane the user is interacting with. The top bar owns library selection
/// (1-9) + search; the four content panes (library items, sections, content,
/// context) take focus for navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    TopBar,
    LibraryItems,
    LibrarySections,
    Content,
    ContextTop,
    ContextBottom,
}

/// A library item (movie, episode, album, …) with the fields the detail pane shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// A side effect requested by the user that the event loop performs (handling
/// the key itself stays pure and I/O-free). Drained each tick via
/// [`App::take_intents`].
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Play this item (already classified as Video or Audio).
    Play { item: Item, media: MediaKind },
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
    /// Refetch the library root level with this section's filter (Enter on a
    /// section). The library's drill stack is reset to a loading root level.
    ApplySection {
        library_id: String,
        section: Section,
    },
    /// Run a search query (Enter inside the search input). Results replace the
    /// active library's root level via `apply_search_results`.
    Search { query: String },
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

/// Cast / crew member shown in the right-column context pane for movies/tv.
#[derive(Debug, Clone, Default)]
pub struct Person {
    pub name: String,
    /// e.g. `Neo`, `Director`, `Writer`. May be empty.
    pub role: Option<String>,
    /// Jellyfin `Type` (`Actor`, `Director`, `Writer`, `GuestStar`, …).
    pub kind: Option<String>,
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
        ],
        Some("tvshows") => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Series", &["Series"], &["SortName"]),
            Section::new("Episodes", &["Episode"], &["DateCreated"]),
        ],
        Some("books") => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Books", &["Book"], &["SortName"]),
            Section::new("Authors", &["Person"], &["SortName"]),
        ],
        Some("photos") => vec![
            Section::new("All", &[], &["SortName"]),
            Section::new("Albums", &["PhotoAlbum"], &["SortName"]),
            Section::new("Photos", &["Photo"], &["DateCreated"]),
        ],
        _ => vec![Section::new("All", &[], &["SortName"])],
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
    /// Transient one-liner in the status bar (e.g. "Not playable"); cleared on the
    /// next key press.
    status_message: Option<String>,
    /// Side effects queued by [`App::handle_key`] for the loop to perform.
    pending: Vec<Intent>,
    error: Option<String>,
    error_copied: bool,
    keymap: Keymap,
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
            status_message: None,
            pending: Vec::new(),
            error: None,
            error_copied: false,
            keymap: Keymap::default(),
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

    pub fn current_item(&self) -> Option<&Item> {
        self.current_level()
            .and_then(|level| level.items.get(level.selected))
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
        if self
            .current_item()
            .is_some_and(|item| item.id == item_id)
        {
            self.current_detail = Some((item_id.to_string(), detail));
        }
    }

    /// Drop any cached detail (called when the selection changes so the
    /// renderer doesn't show stale lyrics/cast).
    pub fn clear_current_detail(&mut self) {
        self.current_detail = None;
    }

    /// Build the audio queue from the active level: every audio item in the
    /// current list, with the focused item as the starting position. Replaces
    /// any prior queue. The returned tuple is `(queue, starting_index)` so
    /// callers can also feed the first track to the engine.
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
        }];
        // Fetch first (the primary action), then queue the save so the choice
        // survives restart.
        self.pending.push(Intent::ApplySection {
            library_id: library.id,
            section,
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
        if let KeyCode::Char(c) = key.code {
            if key.modifiers.is_empty() && c.is_ascii_digit() && c != '0' {
                let index = (c as u8 - b'1') as usize;
                self.status_message = None;
                self.select_library(index);
                return;
            }
        }

        let Some(action) = self.keymap.action_for(key) else {
            return;
        };

        // A new keypress clears any transient status note from the last one.
        self.status_message = None;

        match action {
            Action::Quit => self.should_quit = true,
            Action::Up => self.cursor_up(),
            Action::Down => self.cursor_down(),
            Action::Left => self.focus_prev_or_back(),
            Action::Right => self.focus_next(),
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
            Action::Help => self.show_help = true,
            Action::Cancel => {}
        }
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

    /// Push a loading root level and queue the search fetch.
    fn start_search(&mut self, query: String) {
        let Some(library) = self.current_library().cloned() else {
            return;
        };
        self.stack = vec![Level {
            title: format!("Search: {query}"),
            parent_id: library.id.clone(),
            items: Vec::new(),
            selected: 0,
            loading: true,
        }];
        self.pending.push(Intent::Search { query });
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
        let Some(item) = self.current_item().cloned() else {
            return;
        };
        if item.is_folder {
            self.drill_into(&item);
            return;
        }
        match MediaKind::classify(item.kind.as_deref()) {
            media @ (MediaKind::Video | MediaKind::Audio) => {
                self.pending.push(Intent::Play { item, media });
            }
            MediaKind::Other => {
                self.status_message = Some(format!("Not playable: {}", item.name));
            }
        }
    }

    /// Push a loading level for `item` and queue the fetch of its children.
    fn drill_into(&mut self, item: &Item) {
        self.stack.push(Level {
            title: item.name.clone(),
            parent_id: item.id.clone(),
            items: Vec::new(),
            selected: 0,
            loading: true,
        });
        self.pending.push(Intent::OpenFolder {
            id: item.id.clone(),
            title: item.name.clone(),
        });
    }

    /// Go up one drill level; at a library root, drop focus to the top bar.
    fn go_back(&mut self) {
        if self.is_drilled() {
            self.stack.pop();
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
                if let Some(level) = self.current_level_mut() {
                    if level.selected + 1 < level.items.len() {
                        level.selected += 1;
                    }
                }
            }
            Pane::LibrarySections => {
                let max = self.current_sections().len().saturating_sub(1);
                if self.section_selected < max {
                    self.section_selected += 1;
                }
            }
            Pane::TopBar | Pane::Content | Pane::ContextTop | Pane::ContextBottom => {}
        }
    }

    fn cursor_up(&mut self) {
        match self.focus {
            Pane::LibraryItems => {
                if let Some(level) = self.current_level_mut() {
                    level.selected = level.selected.saturating_sub(1);
                }
            }
            Pane::LibrarySections => {
                self.section_selected = self.section_selected.saturating_sub(1);
            }
            Pane::TopBar | Pane::Content | Pane::ContextTop | Pane::ContextBottom => {}
        }
    }

    fn go_top(&mut self) {
        match self.focus {
            Pane::LibraryItems => {
                if let Some(level) = self.current_level_mut() {
                    level.selected = 0;
                }
            }
            Pane::LibrarySections => self.section_selected = 0,
            _ => {}
        }
    }

    fn go_bottom(&mut self) {
        match self.focus {
            Pane::LibraryItems => {
                if let Some(level) = self.current_level_mut() {
                    level.selected = level.items.len().saturating_sub(1);
                }
            }
            Pane::LibrarySections => {
                self.section_selected = self.current_sections().len().saturating_sub(1);
            }
            _ => {}
        }
    }

    /// Cycle focus forward (Right). Order:
    /// LibraryItems → LibrarySections → Content → ContextTop → ContextBottom.
    fn focus_next(&mut self) {
        self.focus = match self.focus {
            Pane::TopBar | Pane::LibraryItems => Pane::LibrarySections,
            Pane::LibrarySections => Pane::Content,
            Pane::Content => Pane::ContextTop,
            Pane::ContextTop => Pane::ContextBottom,
            Pane::ContextBottom => Pane::ContextBottom,
        };
    }

    /// Cycle focus back (Left). In a drilled list it first walks up the folder
    /// stack (yazi-style) before changing pane.
    fn focus_prev_or_back(&mut self) {
        if self.focus == Pane::LibraryItems && self.is_drilled() {
            self.stack.pop();
            return;
        }
        self.focus = match self.focus {
            Pane::ContextBottom => Pane::ContextTop,
            Pane::ContextTop => Pane::Content,
            Pane::Content => Pane::LibrarySections,
            Pane::LibrarySections => Pane::LibraryItems,
            Pane::LibraryItems | Pane::TopBar => Pane::LibraryItems,
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
    let mut last_item_kind: Option<String> = None;
    let mut stable_ticks: u8 = 0;
    while !app.should_quit {
        // Snapshot the current selection so we can run the stability gate
        // without holding any borrows on `app`.
        let current = app
            .current_item()
            .map(|item| (item.id.clone(), item.kind.clone(), item.primary_image_tag.is_some()));
        let current_id = current.as_ref().map(|(id, _, _)| id.clone());
        let current_kind = current.as_ref().and_then(|(_, k, _)| k.clone());
        let has_art = current.as_ref().is_some_and(|(_, _, art)| *art);

        if current_id == last_item_id {
            stable_ticks = stable_ticks.saturating_add(1);
        } else {
            // Selection moved — drop any cached detail so the renderer doesn't
            // show last item's lyrics/cast while the new fetch is in flight.
            app.clear_current_detail();
            stable_ticks = 0;
            last_item_id = current_id.clone();
            last_item_kind = current_kind.clone();
        }
        let gate_open = stable_ticks >= STABLE_TICKS;

        if let Some(im) = images.as_deref_mut() {
            im.tick();
            if gate_open && has_art {
                if let Some(id) = &current_id {
                    im.request(id);
                }
            }
            // The now-playing cover is always wanted while something plays.
            if let Some(np) = &app.now_playing {
                im.request(&np.item_id);
            }
        }

        if let Some(dt) = details.as_deref_mut() {
            dt.tick(app);
            if gate_open {
                if let Some(id) = &current_id {
                    dt.request(id, last_item_kind.as_deref());
                }
            }
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
                } => {
                    let library_name = app
                        .libraries
                        .iter()
                        .find(|l| l.id == library_id)
                        .map(|l| l.name.clone())
                        .unwrap_or_default();
                    if let Some(br) = browser.as_deref_mut() {
                        br.apply_section(library_id, library_name, section);
                    }
                }
                Intent::Search { query } => {
                    let library_id = app
                        .current_library()
                        .map(|l| l.id.clone())
                        .unwrap_or_default();
                    if !library_id.is_empty() {
                        if let Some(br) = browser.as_deref_mut() {
                            br.search(library_id, query);
                        }
                    }
                }
                Intent::SetTheme(name) => match crate::theme::load(&name) {
                    Ok(theme) => app.set_theme(theme),
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

fn persist_search_history(history: Vec<String>) -> Result<()> {
    let mut config = crate::config::Config::load()?.unwrap_or_default();
    config.ui.search_history = history;
    config.save()
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
        theme,
    );

    panes::library_items::render(
        frame,
        regions.library_items,
        app.current_level(),
        &app.breadcrumb(),
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

    panes::content::render(
        frame,
        regions.content,
        app.current_item(),
        app.focus == Pane::Content,
        images.as_deref_mut(),
        theme,
    );

    let collection_type = app.current_library().and_then(|l| l.collection_type.as_deref());
    let detail = app.current_detail();
    let playback_position = app.now_playing.as_ref().map(|np| np.position);
    panes::context_pane::render_top(
        frame,
        regions.context_top,
        collection_type,
        detail,
        playback_position,
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
    } else if app.show_help {
        cheatsheet::render(frame, area, &app.keymap, theme);
    }
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let focus_name = match app.focus {
        Pane::TopBar => "Libraries",
        Pane::LibraryItems => "Items",
        Pane::LibrarySections => "Sections",
        Pane::Content => "Details",
        Pane::ContextTop => "Context",
        Pane::ContextBottom => "Queue",
    };
    // A transient note (e.g. "Not playable") takes over the left side when set.
    let left = match &app.status_message {
        Some(message) => format!(" {message} "),
        None => {
            let item = app.current_item().map_or("-", |i| i.name.as_str());
            format!(" {focus_name}  ·  {}  ·  {item} ", app.breadcrumb())
        }
    };
    let hint = " Enter open/play · Bksp back · t themes · F1 help · q quit ";

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
    fn focus_cycles_through_panes() {
        // App starts focused on LibraryItems; Right walks through sections,
        // content, and the two context panes; Left walks back.
        let mut app = App::new();
        assert_eq!(app.focus, Pane::LibraryItems);
        app.handle_key(press(KeyCode::Right));
        assert_eq!(app.focus, Pane::LibrarySections);
        app.handle_key(press(KeyCode::Right));
        assert_eq!(app.focus, Pane::Content);
        app.handle_key(press(KeyCode::Right));
        assert_eq!(app.focus, Pane::ContextTop);
        app.handle_key(press(KeyCode::Right));
        assert_eq!(app.focus, Pane::ContextBottom);
        app.handle_key(press(KeyCode::Right)); // clamps at the rightmost pane
        assert_eq!(app.focus, Pane::ContextBottom);
        app.handle_key(press(KeyCode::Left));
        assert_eq!(app.focus, Pane::ContextTop);
        app.handle_key(press(KeyCode::Left));
        assert_eq!(app.focus, Pane::Content);
        app.handle_key(press(KeyCode::Left));
        assert_eq!(app.focus, Pane::LibrarySections);
        app.handle_key(press(KeyCode::Left));
        assert_eq!(app.focus, Pane::LibraryItems);
        app.handle_key(press(KeyCode::Left)); // clamps at the leftmost pane
        assert_eq!(app.focus, Pane::LibraryItems);
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
        app.handle_key(press(KeyCode::Right)); // focus list
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
            is_folder: kind == "Series" || kind == "Season" || kind == "MusicAlbum",
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
    fn enter_queues_play_intent_for_playable_items() {
        let mut app = app_with_item("Movie");
        app.handle_key(press(KeyCode::Enter));
        let intents = app.take_intents();
        assert_eq!(intents.len(), 1);
        assert!(matches!(
            &intents[0],
            Intent::Play { media: MediaKind::Video, item } if item.name == "Thing"
        ));

        let mut app = app_with_item("Audio");
        app.handle_key(press(KeyCode::Enter));
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::Play { media: MediaKind::Audio, .. }]
        ));
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
        // …and an OpenFolder intent is queued for the loader.
        assert!(matches!(
            app.take_intents().as_slice(),
            [Intent::OpenFolder { id, .. }] if id == "id-Thing"
        ));
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
    fn left_in_drilled_list_goes_up_a_level() {
        // Initial focus is LibraryItems; Enter drills into the folder.
        let mut app = app_with_item("Series");
        app.handle_key(press(KeyCode::Enter)); // drill in
        let _ = app.take_intents();
        assert!(app.is_drilled());
        assert_eq!(app.focus, Pane::LibraryItems);
        app.handle_key(press(KeyCode::Left)); // pops the level, keeps list focus
        assert!(!app.is_drilled());
        assert_eq!(app.focus, Pane::LibraryItems);
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
    fn detail_pane_shows_real_metadata() {
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
            }],
        }]);
        let out = rendered(&app, 120, 30);
        assert!(out.contains("Neo learns the truth"));
        assert!(out.contains("1999"));
        assert!(out.contains("Movie"));
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
            Intent::ApplySection { library_id, section } => {
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
            Intent::Search { query } => assert_eq!(query, "matrix"),
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
    fn n_and_p_keys_emit_queue_nav_intents() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('n')));
        app.handle_key(press(KeyCode::Char('p')));
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
                name: "Neo".to_string(),
                role: Some("Hero".to_string()),
                kind: Some("Actor".to_string()),
            }],
            genres: vec!["Sci-Fi".to_string()],
            lyrics: None,
            children: Vec::new(),
            siblings: Vec::new(),
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
        let out = rendered(&app, 120, 40);
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
