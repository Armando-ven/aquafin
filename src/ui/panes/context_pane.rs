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

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::{Item, ItemDetail, LyricLine, NowPlaying, Person, RepeatMode};

pub fn render_top(
    frame: &mut Frame,
    area: Rect,
    collection_type: Option<&str>,
    detail: Option<&ItemDetail>,
    position: Option<Duration>,
    focused: bool,
    theme: &Theme,
) {
    match collection_type {
        Some("music") => render_lyrics(frame, area, detail, position, focused, theme),
        Some("movies") => render_cast(frame, area, detail, focused, theme),
        Some("tvshows") => render_tv_episodes(frame, area, detail, focused, theme),
        _ => render_placeholder(frame, area, " Info ", "Select an item.", focused, theme),
    }
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
            focused,
            theme,
        ),
        Some("movies") => render_credits(frame, area, detail, focused, theme),
        Some("tvshows") => render_tv_seasons(frame, area, detail, focused, theme),
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
    let inner_height = block.inner(area).height as usize;
    let lines: Vec<Line> = match detail.and_then(|d| d.lyrics.as_deref()) {
        Some(lyrics) if !lyrics.is_empty() => render_lyric_lines(lyrics, position, theme),
        Some(_) => vec![Line::from("No lyrics for this track.")],
        None => vec![Line::from("Select a track to view lyrics.")],
    };
    // Auto-scroll so the active line stays roughly centered within the pane.
    let active = detail
        .and_then(|d| d.lyrics.as_deref())
        .filter(|l| !l.is_empty())
        .zip(position)
        .and_then(|(l, p)| active_lyric_index(l, p));
    let scroll = scroll_offset(active, lines.len(), inner_height);
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme.list_item())
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(block),
        area,
    );
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
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(" Cast ")
        .border_style(theme.border(focused));
    let lines: Vec<Line> = match detail.map(|d| d.cast.as_slice()) {
        Some(cast) if !cast.is_empty() => cast
            .iter()
            .filter(|p| p.kind.as_deref().is_none_or(is_cast_kind))
            .map(person_line)
            .collect(),
        Some(_) => vec![Line::from("No cast listed.")],
        None => vec![Line::from("Select an item to load cast.")],
    };
    frame.render_widget(
        Paragraph::new(lines).style(theme.list_item()).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

fn render_credits(
    frame: &mut Frame,
    area: Rect,
    detail: Option<&ItemDetail>,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(" Credits ")
        .border_style(theme.border(focused));
    let mut lines: Vec<Line> = Vec::new();
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
    frame.render_widget(
        Paragraph::new(lines).style(theme.list_item()).wrap(Wrap { trim: true }).block(block),
        area,
    );
}

/// TV context, top pane. For a Series this is the list of seasons; for a
/// Season the episodes; for an Episode the season-mates fetched as siblings.
fn render_tv_episodes(
    frame: &mut Frame,
    area: Rect,
    detail: Option<&ItemDetail>,
    focused: bool,
    theme: &Theme,
) {
    let (title, items, fallback) = pick_tv_top(detail);
    render_item_list(frame, area, title, items, fallback, focused, theme);
}

/// TV context, bottom pane. Crude pairing of "what's next to this": for an
/// Episode the other episodes of the same season; for a Season the other
/// seasons; for a Series the genres line.
fn render_tv_seasons(
    frame: &mut Frame,
    area: Rect,
    detail: Option<&ItemDetail>,
    focused: bool,
    theme: &Theme,
) {
    let (title, items, fallback) = pick_tv_bottom(detail);
    render_item_list(frame, area, title, items, fallback, focused, theme);
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
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(title.to_string())
        .border_style(theme.border(focused));
    let lines: Vec<Line> = if items.is_empty() {
        vec![Line::from(fallback.to_string())]
    } else {
        items
            .iter()
            .map(|item| Line::from(item.name.clone()))
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines).style(theme.list_item()).wrap(Wrap { trim: true }).block(block),
        area,
    );
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
    focused: bool,
    theme: &Theme,
) {
    let title = queue_title(repeat_mode, shuffle);
    let block = Block::bordered()
        .title(title)
        .border_style(theme.border(focused));
    let mut lines: Vec<Line> = Vec::new();
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
    frame.render_widget(Paragraph::new(lines).block(block), area);
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
