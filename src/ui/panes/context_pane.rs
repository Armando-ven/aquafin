//! Right column context panes. Their content depends on the active library
//! kind:
//!
//! - **music**: top = lyrics, bottom = play queue
//! - **movies / tv**: top = cast, bottom = credits (crew + genres)
//! - **other**: empty placeholders
//!
//! Cast and lyrics come from the on-demand [`crate::ui::details::Details`]
//! fetcher, so they only populate once the user lingers on an item.

use std::time::Duration;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::{ContextTopView, Item, ItemDetail, LyricLine, NowPlaying, Person, RepeatMode};
use crate::ui::images::Images;
use crate::ui::panes::content::format_runtime;

pub fn render_top(
    frame: &mut Frame,
    area: Rect,
    collection_type: Option<&str>,
    item: Option<&Item>,
    detail: Option<&ItemDetail>,
    view: ContextTopView,
    scroll: u16,
    revealed: bool,
    position: Option<Duration>,
    images: Option<&mut Images>,
    focused: bool,
    theme: &Theme,
) {
    match collection_type {
        Some("music") => match view {
            ContextTopView::Lyrics => render_lyrics(frame, area, detail, position, focused, theme),
            ContextTopView::Info => render_info(
                frame, area, item, detail, scroll, revealed, images, focused, theme,
            ),
        },
        Some("movies") => render_cast(frame, area, detail, scroll, focused, theme),
        Some("tvshows") => render_tv_episodes(frame, area, detail, scroll, focused, theme),
        _ => render_placeholder(frame, area, " Info ", "Select an item.", focused, theme),
    }
}

/// Music info pane: cover + description + contents. For a MusicArtist the
/// contents are the artist's albums and "appears on"; for a MusicAlbum it's
/// the track list; for an Audio item the sibling tracks on the same album.
fn render_info(
    frame: &mut Frame,
    area: Rect,
    item: Option<&Item>,
    detail: Option<&ItemDetail>,
    scroll: u16,
    revealed: bool,
    images: Option<&mut Images>,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(" Info ")
        .border_style(theme.border(focused));
    let Some(item) = item else {
        frame.render_widget(
            Paragraph::new("Select an item.").style(theme.muted()).block(block),
            area,
        );
        return;
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve a slice of the pane for the cover when the item has art, the
    // user has revealed the item, and the terminal can draw images.
    let has_art = item.primary_image_tag.is_some();
    let can_draw_cover = images.as_ref().is_some_and(|im| im.is_available());
    let text_area = if revealed && has_art && can_draw_cover && inner.height >= 10 {
        let cover_height = (inner.height / 3).clamp(6, 14);
        let [cover_area, text_area] =
            Layout::vertical([Constraint::Length(cover_height), Constraint::Min(0)]).areas(inner);
        if let Some(images) = images {
            images.draw(frame, cover_area, &item.id);
        }
        text_area
    } else {
        inner
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(item.name.clone(), theme.header())));

    let mut meta: Vec<String> = Vec::new();
    if let Some(kind) = &item.kind {
        meta.push(kind.clone());
    }
    if let Some(year) = item.production_year {
        meta.push(year.to_string());
    }
    if let Some(runtime) = item.run_time_ticks.and_then(format_runtime) {
        meta.push(runtime);
    }
    if !meta.is_empty() {
        lines.push(Line::from(Span::styled(meta.join("  ·  "), theme.muted())));
    }
    lines.push(Line::from(""));

    let overview = detail
        .and_then(|d| d.overview.as_deref())
        .or(item.overview.as_deref())
        .filter(|o| !o.is_empty());
    match overview {
        Some(text) => lines.push(Line::from(text.to_string())),
        None => match detail {
            None => lines.push(Line::from(Span::styled("Loading…", theme.muted()))),
            Some(_) => lines.push(Line::from(Span::styled("No description.", theme.muted()))),
        },
    }
    lines.push(Line::from(""));

    if let Some(detail) = detail {
        match item.kind.as_deref() {
            Some("MusicArtist") => {
                push_section(&mut lines, "Albums", &detail.artist_albums, theme);
                push_section(&mut lines, "Appears on", &detail.appears_on, theme);
            }
            Some("MusicAlbum") => {
                push_section(&mut lines, "Tracks", &detail.children, theme);
            }
            Some("Audio" | "AudioBook") => {
                push_section(&mut lines, "Album tracks", &detail.siblings, theme);
            }
            _ => {}
        }
        if !detail.genres.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("Genres: {}", detail.genres.join(", ")),
                theme.muted(),
            )));
        }
    }

    render_scrollable_paragraph(frame, text_area, lines, scroll, theme);
}

/// Render `lines` into `area` as a wrapped paragraph with a right-edge
/// scrollbar that surfaces only when the wrapped content exceeds the viewport.
/// `area` should already exclude the surrounding block's border.
fn render_scrollable_paragraph(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'static>>,
    scroll: u16,
    theme: &Theme,
) {
    let viewport = area.height;
    // Reserve the rightmost column for the scrollbar so wrap accounting
    // matches what's actually drawn.
    let wrap_width = area.width.saturating_sub(1).max(1);
    let paragraph = Paragraph::new(lines)
        .style(theme.list_item())
        .wrap(Wrap { trim: true });
    let content_rows = paragraph.line_count(wrap_width) as u16;
    let max_scroll = content_rows.saturating_sub(viewport);
    let effective_scroll = scroll.min(max_scroll);
    frame.render_widget(paragraph.scroll((effective_scroll, 0)), area);
    if content_rows > viewport && viewport > 0 {
        let mut state = ScrollbarState::new(content_rows as usize)
            .viewport_content_length(viewport as usize)
            .position(effective_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            area,
            &mut state,
        );
    }
}

/// Render `lines` inside a bordered block with title and scrollbar.
fn render_scrollable_block(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    scroll: u16,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(title.to_string())
        .border_style(theme.border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    render_scrollable_paragraph(frame, inner, lines, scroll, theme);
}

/// Append a titled list of items with a ♥ marker for favorites. Skips the
/// section entirely when there's nothing to show.
fn push_section(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    items: &[Item],
    theme: &Theme,
) {
    if items.is_empty() {
        return;
    }
    lines.push(Line::from(Span::styled(title.to_string(), theme.header())));
    for item in items {
        let marker = if item.is_favorite { "♥ " } else { "  " };
        lines.push(Line::from(format!("{marker}{}", item.name)));
    }
    lines.push(Line::from(""));
}

pub fn render_bottom(
    frame: &mut Frame,
    area: Rect,
    collection_type: Option<&str>,
    detail: Option<&ItemDetail>,
    now_playing: Option<&NowPlaying>,
    current_track: Option<&Item>,
    upcoming: &[Item],
    repeat_mode: RepeatMode,
    shuffle: bool,
    scroll: u16,
    focused: bool,
    theme: &Theme,
) {
    match collection_type {
        Some("music") => render_queue(
            frame,
            area,
            now_playing,
            current_track,
            upcoming,
            repeat_mode,
            shuffle,
            scroll,
            focused,
            theme,
        ),
        Some("movies") => render_credits(frame, area, detail, scroll, focused, theme),
        Some("tvshows") => render_tv_seasons(frame, area, detail, scroll, focused, theme),
        _ => render_placeholder(frame, area, " More ", "", focused, theme),
    }
}

fn render_lyrics(
    frame: &mut Frame,
    area: Rect,
    detail: Option<&ItemDetail>,
    position: Option<Duration>,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(" Lyrics ")
        .border_style(theme.border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line<'static>> = match detail.and_then(|d| d.lyrics.as_deref()) {
        Some(lyrics) if !lyrics.is_empty() => render_lyric_lines(lyrics, position, theme),
        Some(_) => vec![Line::from("No lyrics for this track.")],
        None => vec![Line::from(
            "No lyrics loaded. Press p → Show lyrics to fetch.",
        )],
    };
    // Auto-scroll so the active line stays roughly centered within the pane.
    let active = detail
        .and_then(|d| d.lyrics.as_deref())
        .filter(|l| !l.is_empty())
        .zip(position)
        .and_then(|(l, p)| active_lyric_index(l, p));
    let scroll = scroll_offset(active, lines.len(), inner.height as usize);
    render_scrollable_paragraph(frame, inner, lines, scroll, theme);
}

/// Pick a vertical scroll offset so the active line sits near the middle of the
/// pane. Clamps to the legal range so the last lines stay flush at the bottom.
fn scroll_offset(active: Option<usize>, total: usize, viewport: usize) -> u16 {
    let Some(active) = active else { return 0 };
    if viewport == 0 || total <= viewport {
        return 0;
    }
    let half = viewport / 2;
    let max_scroll = total - viewport;
    active.saturating_sub(half).min(max_scroll) as u16
}

/// Convert lyric lines into renderable [`Line`]s. When the lyrics are synced
/// (lines carry `start_ticks`) and we know the current playback position, the
/// active line is highlighted with the theme's header style.
fn render_lyric_lines(
    lyrics: &[LyricLine],
    position: Option<Duration>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let active = position
        .and_then(|pos| active_lyric_index(lyrics, pos))
        .unwrap_or(usize::MAX);
    lyrics
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if i == active {
                Line::from(Span::styled(line.text.clone(), theme.header()))
            } else {
                Line::from(line.text.clone())
            }
        })
        .collect()
}

/// Index of the lyric line whose `start_ticks` is the greatest value still
/// ≤ the current position. `None` when lyrics aren't synced or the position
/// precedes the first timestamped line.
fn active_lyric_index(lyrics: &[LyricLine], position: Duration) -> Option<usize> {
    // Jellyfin reports start in 100 ns ticks.
    let position_ticks = (position.as_nanos() / 100) as i64;
    let mut best: Option<usize> = None;
    for (i, line) in lyrics.iter().enumerate() {
        match line.start_ticks {
            Some(start) if start <= position_ticks => best = Some(i),
            Some(_) => break,
            None => {}
        }
    }
    best
}

fn render_cast(
    frame: &mut Frame,
    area: Rect,
    detail: Option<&ItemDetail>,
    scroll: u16,
    focused: bool,
    theme: &Theme,
) {
    let lines: Vec<Line<'static>> = match detail.map(|d| d.cast.as_slice()) {
        Some(cast) if !cast.is_empty() => cast
            .iter()
            .filter(|p| p.kind.as_deref().is_none_or(is_cast_kind))
            .map(person_line)
            .collect(),
        Some(_) => vec![Line::from("No cast listed.")],
        None => vec![Line::from("Select an item to load cast.")],
    };
    render_scrollable_block(frame, area, " Cast ", lines, scroll, focused, theme);
}

fn render_credits(
    frame: &mut Frame,
    area: Rect,
    detail: Option<&ItemDetail>,
    scroll: u16,
    focused: bool,
    theme: &Theme,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(detail) = detail {
        if !detail.genres.is_empty() {
            lines.push(Line::from(format!("Genres: {}", detail.genres.join(", "))));
            lines.push(Line::from(""));
        }
        for person in &detail.cast {
            if person.kind.as_deref().is_some_and(is_crew_kind) {
                lines.push(person_line(person));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("No credits loaded."));
    }
    render_scrollable_block(frame, area, " Credits ", lines, scroll, focused, theme);
}

/// TV context, top pane. For a Series this is the list of seasons; for a
/// Season the episodes; for an Episode the season-mates fetched as siblings.
fn render_tv_episodes(
    frame: &mut Frame,
    area: Rect,
    detail: Option<&ItemDetail>,
    scroll: u16,
    focused: bool,
    theme: &Theme,
) {
    let (title, items, fallback) = pick_tv_top(detail);
    render_item_list(frame, area, title, items, fallback, scroll, focused, theme);
}

/// TV context, bottom pane. Crude pairing of "what's next to this": for an
/// Episode the other episodes of the same season; for a Season the other
/// seasons; for a Series the genres line.
fn render_tv_seasons(
    frame: &mut Frame,
    area: Rect,
    detail: Option<&ItemDetail>,
    scroll: u16,
    focused: bool,
    theme: &Theme,
) {
    let (title, items, fallback) = pick_tv_bottom(detail);
    render_item_list(frame, area, title, items, fallback, scroll, focused, theme);
}

fn pick_tv_top(detail: Option<&ItemDetail>) -> (&'static str, Vec<&Item>, &'static str) {
    let Some(detail) = detail else {
        return (" Episodes ", Vec::new(), "Select an item.");
    };
    if !detail.children.is_empty() {
        let title = if detail.children.iter().all(|i| i.kind.as_deref() == Some("Season")) {
            " Seasons "
        } else {
            " Episodes "
        };
        return (title, detail.children.iter().collect(), "");
    }
    if !detail.siblings.is_empty() {
        return (" Episodes ", detail.siblings.iter().collect(), "");
    }
    (" Episodes ", Vec::new(), "No episodes loaded.")
}

fn pick_tv_bottom(detail: Option<&ItemDetail>) -> (&'static str, Vec<&Item>, &'static str) {
    let Some(detail) = detail else {
        return (" Seasons ", Vec::new(), "");
    };
    // Season selected → other seasons via siblings.
    if !detail.siblings.is_empty()
        && detail.siblings.iter().all(|i| i.kind.as_deref() == Some("Season"))
    {
        return (" Seasons ", detail.siblings.iter().collect(), "");
    }
    (" More ", Vec::new(), "")
}

fn render_item_list(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    items: Vec<&Item>,
    fallback: &str,
    scroll: u16,
    focused: bool,
    theme: &Theme,
) {
    let lines: Vec<Line<'static>> = if items.is_empty() {
        vec![Line::from(fallback.to_string())]
    } else {
        items
            .iter()
            .map(|item| Line::from(item.name.clone()))
            .collect()
    };
    render_scrollable_block(frame, area, title, lines, scroll, focused, theme);
}

/// Music queue: current track from the queue state, then up to a screenful of
/// upcoming tracks. Falls back to the now-playing snapshot when the queue is
/// empty (e.g. audio resumed from a non-list source).
fn render_queue(
    frame: &mut Frame,
    area: Rect,
    now_playing: Option<&NowPlaying>,
    current_track: Option<&Item>,
    upcoming: &[Item],
    repeat_mode: RepeatMode,
    shuffle: bool,
    scroll: u16,
    focused: bool,
    theme: &Theme,
) {
    let title = queue_title(repeat_mode, shuffle);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let current_name: Option<String> = current_track
        .map(|t| t.name.clone())
        .or_else(|| now_playing.map(|np| np.title.clone()));
    match current_name {
        Some(name) => {
            lines.push(Line::from(Span::styled(format!("▶ {name}"), theme.header())));
            if let Some(sub) = now_playing.and_then(|np| np.subtitle.as_ref()) {
                lines.push(Line::from(Span::styled(sub.clone(), theme.muted())));
            }
            lines.push(Line::from(""));
            if upcoming.is_empty() {
                lines.push(Line::from(Span::styled("Up next: —", theme.muted())));
            } else {
                lines.push(Line::from(Span::styled("Up next:", theme.muted())));
                for track in upcoming {
                    lines.push(Line::from(format!("  {}", track.name)));
                }
            }
        }
        None => lines.push(Line::from(Span::styled("Queue is empty.", theme.muted()))),
    }
    render_scrollable_block(frame, area, &title, lines, scroll, focused, theme);
}

/// Build the queue block title, surfacing any non-default modes so the user
/// can see at a glance what shuffle/repeat are doing.
fn queue_title(repeat_mode: RepeatMode, shuffle: bool) -> String {
    let mut flags: Vec<&str> = Vec::new();
    if shuffle {
        flags.push("Shuffle");
    }
    if repeat_mode != RepeatMode::Off {
        match repeat_mode {
            RepeatMode::All => flags.push("Repeat all"),
            RepeatMode::One => flags.push("Repeat one"),
            RepeatMode::Off => {}
        }
    }
    if flags.is_empty() {
        " Queue ".to_string()
    } else {
        format!(" Queue · {} ", flags.join(" · "))
    }
}

fn render_placeholder(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    body: &str,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(title.to_string())
        .border_style(theme.border(focused));
    frame.render_widget(Paragraph::new(body).style(theme.muted()).block(block), area);
}

fn is_cast_kind(kind: &str) -> bool {
    matches!(kind, "Actor" | "GuestStar")
}

fn is_crew_kind(kind: &str) -> bool {
    matches!(kind, "Director" | "Writer" | "Producer" | "Composer")
}

fn person_line(person: &Person) -> Line<'static> {
    match person.role.as_deref().filter(|r| !r.is_empty()) {
        Some(role) => Line::from(format!("{}  —  {role}", person.name)),
        None => match person.kind.as_deref() {
            Some(kind) if !kind.is_empty() => Line::from(format!("{}  ({kind})", person.name)),
            _ => Line::from(person.name.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, ticks: Option<i64>) -> LyricLine {
        LyricLine {
            text: text.to_string(),
            start_ticks: ticks,
        }
    }

    #[test]
    fn active_lyric_index_picks_greatest_start_at_or_before_position() {
        let lyrics = vec![
            line("intro", Some(0)),
            line("verse", Some(30_000_000)),  // 3s
            line("chorus", Some(120_000_000)), // 12s
        ];
        assert_eq!(active_lyric_index(&lyrics, Duration::from_secs(0)), Some(0));
        assert_eq!(active_lyric_index(&lyrics, Duration::from_secs(5)), Some(1));
        assert_eq!(active_lyric_index(&lyrics, Duration::from_secs(20)), Some(2));
    }

    #[test]
    fn active_lyric_index_returns_none_before_first_line() {
        let lyrics = vec![line("verse", Some(30_000_000))];
        assert_eq!(active_lyric_index(&lyrics, Duration::from_secs(0)), None);
    }

    #[test]
    fn active_lyric_index_returns_none_for_untimed_lyrics() {
        let lyrics = vec![line("plain", None), line("text", None)];
        assert_eq!(active_lyric_index(&lyrics, Duration::from_secs(5)), None);
    }

    #[test]
    fn scroll_offset_centers_active_line_when_room() {
        // 40 lines, 10-row viewport, active = 20 → scroll = 15 so the active
        // line sits around the middle.
        assert_eq!(scroll_offset(Some(20), 40, 10), 15);
    }

    #[test]
    fn scroll_offset_clamps_to_end_of_lyrics() {
        // Active near the end shouldn't scroll past the last possible offset.
        assert_eq!(scroll_offset(Some(38), 40, 10), 30);
    }

    #[test]
    fn scroll_offset_returns_zero_when_lyrics_fit_or_no_active() {
        assert_eq!(scroll_offset(Some(2), 5, 10), 0);
        assert_eq!(scroll_offset(None, 40, 10), 0);
    }
}
