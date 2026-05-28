//! Application state and the main TUI event loop.

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    List,
    Detail,
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

/// A library (sidebar entry) and its top-level items.
#[derive(Debug, Clone)]
pub struct Library {
    /// Jellyfin view id (used as the parent id of its top-level items).
    pub id: String,
    pub name: String,
    pub items: Vec<Item>,
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
    pub sidebar_selected: usize,
    /// The list pane's drill-down stack. Never empty while a library exists:
    /// `stack[0]` is the selected library's top-level items.
    pub stack: Vec<Level>,
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
            focus: Pane::Sidebar,
            libraries,
            sidebar_selected: 0,
            stack,
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
        self.libraries.get(self.sidebar_selected)
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
    /// sidebar selection changes; drilling state is intentionally reset).
    fn reset_stack_for_library(&mut self) {
        self.stack = root_level(self.current_library());
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
            Action::Favorite => self.toggle_favorite(),
            Action::Themes => self.open_theme_picker(),
            Action::Help => self.show_help = true,
            Action::Cancel => {}
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

    /// Enter on the focused item: drill into a folder, play a leaf, or note that
    /// it can't be played.
    fn activate(&mut self) {
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

    /// Go up one drill level; at a library root, fall back to the sidebar.
    fn go_back(&mut self) {
        if self.is_drilled() {
            self.stack.pop();
        } else {
            self.focus = Pane::Sidebar;
        }
    }

    /// Drain the queued side effects for the event loop to perform.
    pub fn take_intents(&mut self) -> Vec<Intent> {
        std::mem::take(&mut self.pending)
    }

    /// Set the transient status-bar note (used by the loop for playback feedback).
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
    }

    fn cursor_down(&mut self) {
        match self.focus {
            Pane::Sidebar => {
                if self.sidebar_selected + 1 < self.libraries.len() {
                    self.sidebar_selected += 1;
                    self.reset_stack_for_library();
                }
            }
            Pane::List => {
                if let Some(level) = self.current_level_mut() {
                    if level.selected + 1 < level.items.len() {
                        level.selected += 1;
                    }
                }
            }
            Pane::Detail => {}
        }
    }

    fn cursor_up(&mut self) {
        match self.focus {
            Pane::Sidebar => {
                if self.sidebar_selected > 0 {
                    self.sidebar_selected -= 1;
                    self.reset_stack_for_library();
                }
            }
            Pane::List => {
                if let Some(level) = self.current_level_mut() {
                    level.selected = level.selected.saturating_sub(1);
                }
            }
            Pane::Detail => {}
        }
    }

    fn go_top(&mut self) {
        match self.focus {
            Pane::Sidebar => {
                self.sidebar_selected = 0;
                self.reset_stack_for_library();
            }
            Pane::List => {
                if let Some(level) = self.current_level_mut() {
                    level.selected = 0;
                }
            }
            Pane::Detail => {}
        }
    }

    fn go_bottom(&mut self) {
        match self.focus {
            Pane::Sidebar => {
                self.sidebar_selected = self.libraries.len().saturating_sub(1);
                self.reset_stack_for_library();
            }
            Pane::List => {
                if let Some(level) = self.current_level_mut() {
                    level.selected = level.items.len().saturating_sub(1);
                }
            }
            Pane::Detail => {}
        }
    }

    fn focus_next(&mut self) {
        self.focus = match self.focus {
            Pane::Sidebar => Pane::List,
            Pane::List | Pane::Detail => Pane::Detail,
        };
    }

    /// Left moves focus toward the sidebar, but in a drilled list it first walks
    /// back up the folder stack (yazi-style).
    fn focus_prev_or_back(&mut self) {
        if self.focus == Pane::List && self.is_drilled() {
            self.stack.pop();
        } else {
            self.focus = match self.focus {
                Pane::Detail => Pane::List,
                Pane::List | Pane::Sidebar => Pane::Sidebar,
            };
        }
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
) -> Result<()> {
    const TICK: std::time::Duration = std::time::Duration::from_millis(200);
    while !app.should_quit {
        // Keep covers flowing: collect finished downloads, then request the
        // images the current frame will want.
        if let Some(im) = images.as_deref_mut() {
            im.tick();
            if let Some(item) = app.current_item() {
                if item.primary_image_tag.is_some() {
                    im.request(&item.id);
                }
            }
            if let Some(np) = &app.now_playing {
                im.request(&np.item_id);
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
                Intent::SetTheme(name) => match crate::theme::load(&name) {
                    Ok(theme) => app.set_theme(theme),
                    Err(e) => {
                        tracing::warn!(theme = %name, error = %e, "couldn't load theme");
                        app.show_error(format!("Couldn't load theme \"{name}\": {e}"));
                    }
                },
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

pub fn render(frame: &mut Frame, app: &App, mut images: Option<&mut super::images::Images>) {
    let area = frame.area();
    let regions = layout::compute(area);
    let theme = &app.theme;

    panes::sidebar::render(
        frame,
        regions.sidebar,
        &app.libraries,
        app.focus == Pane::Sidebar,
        app.sidebar_selected,
        theme,
    );

    panes::list::render(
        frame,
        regions.list,
        app.current_level(),
        &app.breadcrumb(),
        app.focus == Pane::List,
        theme,
    );

    panes::detail::render(
        frame,
        regions.detail,
        app.current_item(),
        app.focus == Pane::Detail,
        images.as_deref_mut(),
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
        Pane::Sidebar => "Libraries",
        Pane::List => "Items",
        Pane::Detail => "Details",
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
        let mut app = App::new();
        assert_eq!(app.focus, Pane::Sidebar);
        app.handle_key(press(KeyCode::Right));
        assert_eq!(app.focus, Pane::List);
        app.handle_key(press(KeyCode::Right));
        assert_eq!(app.focus, Pane::Detail);
        app.handle_key(press(KeyCode::Right)); // clamps at the rightmost pane
        assert_eq!(app.focus, Pane::Detail);
        app.handle_key(press(KeyCode::Left));
        assert_eq!(app.focus, Pane::List);
        app.handle_key(press(KeyCode::Left));
        assert_eq!(app.focus, Pane::Sidebar);
        app.handle_key(press(KeyCode::Left)); // clamps at the leftmost pane
        assert_eq!(app.focus, Pane::Sidebar);
    }

    #[test]
    fn arrow_keys_move_selection() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.sidebar_selected, 1);
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.sidebar_selected, 0);
        app.handle_key(press(KeyCode::Up)); // clamps at 0
        assert_eq!(app.sidebar_selected, 0);
    }

    #[test]
    fn switching_library_resets_list_cursor() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Right)); // focus the item list
        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.current_level().unwrap().selected, 1);
        app.handle_key(press(KeyCode::Left)); // back to sidebar
        app.handle_key(press(KeyCode::Down)); // change library
        assert_eq!(app.sidebar_selected, 1);
        assert_eq!(app.current_level().unwrap().selected, 0);
    }

    #[test]
    fn home_and_end_jump_top_and_bottom() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::End));
        assert_eq!(app.sidebar_selected, app.libraries.len() - 1);
        app.handle_key(press(KeyCode::Home));
        assert_eq!(app.sidebar_selected, 0);
    }

    #[test]
    fn f1_opens_help_and_any_key_closes_it() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::F(1)));
        assert!(app.show_help);
        app.handle_key(press(KeyCode::Down)); // any key closes; should not also move
        assert!(!app.show_help);
        assert_eq!(app.sidebar_selected, 0);
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
        app.handle_key(press(KeyCode::Right)); // focus list
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
        app.handle_key(press(KeyCode::Right));
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
        let mut app = app_with_item("Series");
        app.handle_key(press(KeyCode::Right)); // focus list
        app.handle_key(press(KeyCode::Enter)); // drill in
        let _ = app.take_intents();
        assert!(app.is_drilled());
        assert_eq!(app.focus, Pane::List);
        app.handle_key(press(KeyCode::Left)); // pops the level, keeps list focus
        assert!(!app.is_drilled());
        assert_eq!(app.focus, Pane::List);
        app.handle_key(press(KeyCode::Left)); // now moves focus to sidebar
        assert_eq!(app.focus, Pane::Sidebar);
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
    fn renders_three_panes_and_status_bar() {
        let out = rendered(&App::new(), 100, 30);
        assert!(out.contains("Libraries")); // sidebar title
        assert!(out.contains("Details")); // detail pane title
        assert!(out.contains("Movies")); // sidebar entry + list breadcrumb
        assert!(out.contains("The Matrix")); // a list item from the selected library
        assert!(out.contains("F1 help"));
    }

    #[test]
    fn detail_pane_shows_real_metadata() {
        let app = App::with_libraries(vec![Library {
            id: "movies".to_string(),
            name: "Movies".to_string(),
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
        let out = rendered(&app, 100, 30);
        assert!(out.contains("Keybindings"));
        assert!(out.contains("Navigation"));
        assert!(out.contains("Quit"));
    }

    #[test]
    fn error_modal_captures_input_until_dismissed() {
        let mut app = App::new();
        app.show_error("network unreachable");
        assert!(app.error.is_some());
        // Navigation keys are swallowed while the modal is open.
        app.handle_key(press(KeyCode::Down));
        assert!(app.error.is_some());
        assert_eq!(app.sidebar_selected, 0);
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
}
