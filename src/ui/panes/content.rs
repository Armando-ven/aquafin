//! Content pane (middle column): reserved for actual content — a drilled-in
//! album's tracks, a series' episodes, a playlist's songs, etc. The focused
//! item's cover, title, and description live in the info pane (right column
//! top); this pane stays a placeholder until the user drills in.

use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::{
    options_cursor_positions, Item, Level, MediaKind, MediaOptionsCursor, MediaOptionsViewState,
};
use crate::ui::panes::item_kind_decor;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    drilled: Option<&Level>,
    focused: bool,
    theme: &Theme,
) {
    if let Some(level) = drilled {
        render_drilled(frame, area, level, focused, theme);
        return;
    }
    render_placeholder(frame, area, focused, theme);
}

fn render_placeholder(frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
    let block = Block::bordered()
        .title(" Content ")
        .border_style(theme.border(focused));
    frame.render_widget(
        Paragraph::new("Select an album, playlist, or series, then press Enter to view its contents.")
            .style(theme.muted())
            .wrap(Wrap { trim: true })
            .block(block),
        area,
    );
}

fn render_drilled(frame: &mut Frame, area: Rect, level: &Level, focused: bool, theme: &Theme) {
    // Append item count once the level has loaded and has content.
    let count_suffix = if !level.loading && !level.items.is_empty() {
        format!(" ({})", level.items.len())
    } else {
        String::new()
    };
    let block = Block::bordered()
        .title(format!(" {}{count_suffix} ", level.title))
        .border_style(theme.border(focused));

    if level.loading {
        frame.render_widget(
            Paragraph::new("Loading…").style(theme.muted()).block(block),
            area,
        );
        return;
    }
    if level.items.is_empty() {
        frame.render_widget(
            Paragraph::new("Empty.").style(theme.muted()).block(block),
            area,
        );
        return;
    }

    // Music tracks get a multi-column tracklist; everything else stays a list.
    if is_all_audio(&level.items) {
        render_track_table(frame, area, level, focused, theme, block);
        return;
    }

    let list_items: Vec<ListItem> = level
        .items
        .iter()
        .map(|item| {
            let decor = item_kind_decor(item, theme);
            ListItem::new(Line::from(decor.line_spans(item)))
        })
        .collect();

    let list = List::new(list_items)
        .block(block)
        .style(theme.list_item())
        .highlight_style(theme.selected_item(focused))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(level.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn is_all_audio(items: &[Item]) -> bool {
    !items.is_empty()
        && items
            .iter()
            .all(|i| matches!(MediaKind::classify(i.kind.as_deref()), MediaKind::Audio))
}

/// Render the drilled-in level as a music tracklist with columns:
/// `#` (track number), `Title`, `Plays` (play count), `♥` (favorite marker),
/// `Length` (formatted runtime).
fn render_track_table(
    frame: &mut Frame,
    area: Rect,
    level: &Level,
    focused: bool,
    theme: &Theme,
    block: Block<'_>,
) {
    let header = Row::new(vec![
        Cell::from("  #"),
        Cell::from("Title"),
        Cell::from("Plays"),
        Cell::from("♥"),
        Cell::from("Length"),
    ])
    .style(theme.header())
    .height(1);

    let rows: Vec<Row> = level
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let number = item
                .track_number
                .map(|n| format!("{n:>3}"))
                .unwrap_or_else(|| format!("{:>3}", i + 1));
            let plays = if item.play_count > 0 {
                item.play_count.to_string()
            } else {
                "—".to_string()
            };
            let fav = if item.is_favorite { "♥" } else { " " };
            let length = item
                .run_time_ticks
                .and_then(format_track_length)
                .unwrap_or_else(|| "—".to_string());
            Row::new(vec![
                Cell::from(number).style(theme.muted()),
                Cell::from(item.name.clone()),
                Cell::from(plays).style(theme.muted()),
                Cell::from(fav),
                Cell::from(length).style(theme.muted()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(5),
        Constraint::Min(10),
        Constraint::Length(7),
        Constraint::Length(3),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .style(theme.list_item())
        .row_highlight_style(theme.selected_item(focused))
        .highlight_symbol("› ");

    let mut state = TableState::default();
    state.select(Some(level.selected));
    frame.render_stateful_widget(table, area, &mut state);
}

/// Jellyfin `RunTimeTicks` → `m:ss` (or `h:mm:ss` past an hour). Used for the
/// `Length` column in the music tracklist.
fn format_track_length(ticks: i64) -> Option<String> {
    if ticks <= 0 {
        return None;
    }
    let total = (ticks / 10_000_000) as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    Some(if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    })
}

/// Pre-play media options for a video item: version + audio + subtitles +
/// Play. The cursor sits on one selectable row at a time; Enter on a track
/// row commits it; Enter on Play kicks off mpv with the chosen options.
pub fn render_media_options(
    frame: &mut Frame,
    area: Rect,
    view: &MediaOptionsViewState,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(format!(" {} — Play options ", view.item_name))
        .border_style(theme.border(focused));
    if view.loading {
        frame.render_widget(
            Paragraph::new("Loading playback info…")
                .style(theme.muted())
                .block(block),
            area,
        );
        return;
    }

    let positions = options_cursor_positions(view);
    let cursor_index = positions.iter().position(|c| *c == view.cursor).unwrap_or(0);

    let mut items: Vec<ListItem> = Vec::new();
    let mut row_to_cursor: Vec<Option<usize>> = Vec::new();

    // Version section (only shown when there's more than one).
    if view.versions.len() > 1 {
        items.push(section_header("Version", theme));
        row_to_cursor.push(None);
        for (i, v) in view.versions.iter().enumerate() {
            let marker = if i == view.selected_version { "●" } else { "○" };
            items.push(ListItem::new(format!("  {marker}  {}", v.label)));
            row_to_cursor.push(positions.iter().position(|c| *c == MediaOptionsCursor::Version(i)));
        }
    }

    items.push(section_header("Audio", theme));
    row_to_cursor.push(None);
    for (i, e) in view.audio_entries.iter().enumerate() {
        let marker = if i == view.selected_audio { "●" } else { "○" };
        items.push(ListItem::new(format!("  {marker}  {}", e.label)));
        row_to_cursor.push(positions.iter().position(|c| *c == MediaOptionsCursor::Audio(i)));
    }

    items.push(section_header("Subtitles", theme));
    row_to_cursor.push(None);
    for (i, e) in view.subtitle_entries.iter().enumerate() {
        let marker = if i == view.selected_subtitle { "●" } else { "○" };
        items.push(ListItem::new(format!("  {marker}  {}", e.label)));
        row_to_cursor.push(positions.iter().position(|c| *c == MediaOptionsCursor::Subtitle(i)));
    }

    items.push(ListItem::new(""));
    row_to_cursor.push(None);
    items.push(ListItem::new("  ▶  Play").style(theme.header()));
    row_to_cursor.push(positions.iter().position(|c| matches!(c, MediaOptionsCursor::Play)));
    if !view.trailer_urls.is_empty() {
        items.push(ListItem::new("  ▶  Watch trailer").style(theme.header()));
        row_to_cursor
            .push(positions.iter().position(|c| matches!(c, MediaOptionsCursor::WatchTrailer)));
    }

    if !view.chapters.is_empty() {
        items.push(ListItem::new(""));
        row_to_cursor.push(None);
        items.push(section_header("Chapters", theme));
        row_to_cursor.push(None);
        for (i, c) in view.chapters.iter().enumerate() {
            let stamp = format_chapter_timestamp(c.start_position_ticks);
            items.push(ListItem::new(format!("  ▶  {stamp}  {}", c.name)));
            row_to_cursor
                .push(positions.iter().position(|p| *p == MediaOptionsCursor::Chapter(i)));
        }
    }

    let row = row_to_cursor
        .iter()
        .position(|c| matches!(c, Some(i) if *i == cursor_index));

    let list = List::new(items)
        .block(block)
        .style(theme.list_item())
        .highlight_style(theme.selected_item(focused))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(row);
    frame.render_stateful_widget(list, area, &mut state);
}

/// Convert Jellyfin ticks to `h:mm:ss` / `m:ss` for chapter rows.
fn format_chapter_timestamp(ticks: i64) -> String {
    let total_secs = (ticks.max(0) / 10_000_000) as u64;
    let (h, m, s) = (total_secs / 3600, (total_secs % 3600) / 60, total_secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn section_header(title: &str, theme: &Theme) -> ListItem<'static> {
    ListItem::new(Line::from(Span::styled(title.to_string(), theme.header())))
}

/// Jellyfin `RunTimeTicks` (100 ns units) → a short `1h 56m` / `42m` string.
/// Used by the info pane.
pub(crate) fn format_runtime(ticks: i64) -> Option<String> {
    if ticks <= 0 {
        return None;
    }
    let total_minutes = ticks / 10_000_000 / 60;
    let (hours, minutes) = (total_minutes / 60, total_minutes % 60);
    Some(if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    })
}
