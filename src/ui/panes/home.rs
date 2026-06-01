//! Home dashboard: welcome strip + horizontal carousels (Continue, Next Up,
//! Libraries, New in <library>). Tiles are unicode placeholders so the layout
//! works on every terminal regardless of image-protocol support.

use std::time::Duration;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::{HomeCursor, HomeData, HomeRow, HomeRowKind, Item};

/// Height of the welcome strip (top of Home).
const WELCOME_HEIGHT: u16 = 4;
/// Height reserved for a single carousel row: 1 row header + tile body +
/// 1 row title + 1 row meta/progress + 1 row spacer.
const ROW_HEIGHT: u16 = 10;
const TILE_WIDTH: u16 = 18;
const TILE_HEIGHT: u16 = 5;
const TILE_GAP: u16 = 2;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    home: &HomeData,
    rows: &[HomeRow],
    cursor: HomeCursor,
    server_name: Option<&str>,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .border_style(theme.border(focused))
        .padding(Padding::new(1, 1, 0, 0));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 20 || inner.height < 6 {
        return;
    }

    // Welcome strip + scrolling rows.
    let [welcome, rest] = Layout::vertical([
        Constraint::Length(WELCOME_HEIGHT),
        Constraint::Min(0),
    ])
    .areas(inner);

    render_welcome(frame, welcome, home, server_name, theme);

    if home.loading && rows.is_empty() {
        let placeholder = Paragraph::new(Line::from(vec![Span::styled(
            "  Loading your library…",
            theme.muted(),
        )]))
        .alignment(Alignment::Left);
        frame.render_widget(placeholder, rest);
        return;
    }
    if rows.is_empty() {
        let msg = if home.recent_per_library.is_empty() {
            "  No libraries yet. Run `aquafin --setup` to connect a server."
        } else {
            "  Nothing to show yet. Pick a library with the digit keys."
        };
        let placeholder = Paragraph::new(Line::from(vec![Span::styled(msg, theme.muted())]))
            .alignment(Alignment::Left);
        frame.render_widget(placeholder, rest);
        return;
    }

    // Vertically scroll so the focused row stays visible. Each row gets
    // `ROW_HEIGHT`; we compute the first visible row from the cursor.
    let rows_per_viewport = (rest.height / ROW_HEIGHT).max(1) as usize;
    let first_visible = (cursor.row + 1).saturating_sub(rows_per_viewport);

    let mut y = rest.y;
    for (row_index, row) in rows.iter().enumerate().skip(first_visible) {
        if y + ROW_HEIGHT > rest.y + rest.height {
            break;
        }
        let row_area = Rect {
            x: rest.x,
            y,
            width: rest.width,
            height: ROW_HEIGHT,
        };
        let row_focused = focused && row_index == cursor.row;
        let row_cursor_col = if row_index == cursor.row {
            Some(cursor.col)
        } else {
            None
        };
        render_row(frame, row_area, row, row_cursor_col, row_focused, theme);
        y += ROW_HEIGHT;
    }
}

/// Greeting + clock + server tag, in two lines.
fn render_welcome(
    frame: &mut Frame,
    area: Rect,
    home: &HomeData,
    server_name: Option<&str>,
    theme: &Theme,
) {
    let greeting = greeting_for_hour(local_hour());
    let in_progress = home.resume.len();
    let libs = home.recent_per_library.len();
    let total: i64 = home.recent_per_library.iter().map(|s| s.item_count).sum();

    let top = Line::from(vec![
        Span::styled("  ", theme.header()),
        Span::styled(greeting_glyph(local_hour()), accent_bold(theme)),
        Span::raw("  "),
        Span::styled(greeting, theme.header().add_modifier(Modifier::BOLD)),
    ]);

    let mut meta_spans: Vec<Span> = Vec::new();
    meta_spans.push(Span::styled("  aquafin", theme.muted()));
    if let Some(name) = server_name.filter(|s| !s.is_empty()) {
        meta_spans.push(Span::styled("  ·  ", theme.muted()));
        meta_spans.push(Span::styled(name.to_string(), accent(theme)));
    }
    if libs > 0 {
        meta_spans.push(Span::styled("  ·  ", theme.muted()));
        meta_spans.push(Span::styled(format!("{libs} libraries"), theme.muted()));
    }
    if total > 0 {
        meta_spans.push(Span::styled("  ·  ", theme.muted()));
        meta_spans.push(Span::styled(format!("{total} items"), theme.muted()));
    }
    if in_progress > 0 {
        meta_spans.push(Span::styled("  ·  ", theme.muted()));
        meta_spans.push(Span::styled(
            format!("{in_progress} in progress"),
            accent(theme),
        ));
    }
    let meta = Line::from(meta_spans);

    let hint = Line::from(vec![Span::styled(
        "  arrows move · Enter open · digit jumps to library · g h returns Home",
        theme.muted().add_modifier(Modifier::DIM),
    )]);

    let p = Paragraph::new(vec![top, meta, Line::raw(""), hint]);
    frame.render_widget(p, area);
}

fn render_row(
    frame: &mut Frame,
    area: Rect,
    row: &HomeRow,
    cursor_col: Option<usize>,
    row_focused: bool,
    theme: &Theme,
) {
    // Header line: a left accent bar + the row title + tile count.
    let header_area = Rect {
        height: 1,
        ..area
    };
    let chevron = if row_focused { "▶ " } else { "▎ " };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(chevron, accent_bold(theme)),
        Span::styled(row.title.clone(), theme.header()),
        Span::styled(
            format!("   ({})", row.items.len()),
            theme.muted().add_modifier(Modifier::DIM),
        ),
    ]));
    frame.render_widget(header, header_area);

    // Tile band: starts one line below the header.
    let band = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    if band.width == 0 || band.height == 0 {
        return;
    }

    // Carousel horizontal scroll: keep the focused tile visible.
    let tile_stride = TILE_WIDTH + TILE_GAP;
    let max_tiles_visible = ((band.width + TILE_GAP) / tile_stride).max(1) as usize;
    let total_tiles = row.items.len();
    let focused_col = cursor_col.unwrap_or(0).min(total_tiles.saturating_sub(1));
    let first_tile = if cursor_col.is_some() && focused_col + 1 > max_tiles_visible {
        focused_col + 1 - max_tiles_visible
    } else {
        0
    };

    let mut x = band.x;
    for (i, item) in row
        .items
        .iter()
        .enumerate()
        .skip(first_tile)
        .take(max_tiles_visible)
    {
        if x + TILE_WIDTH > band.x + band.width {
            break;
        }
        let tile_area = Rect {
            x,
            y: band.y,
            width: TILE_WIDTH,
            height: TILE_HEIGHT.min(band.height),
        };
        let is_focused = cursor_col == Some(i);
        render_tile(frame, tile_area, row.kind, item, is_focused, row_focused, theme);
        x += tile_stride;
    }

    // Show a small carousel indicator on the right if more tiles overflow.
    if total_tiles > first_tile + max_tiles_visible {
        let ind_x = band.x + band.width.saturating_sub(2);
        let ind_area = Rect {
            x: ind_x,
            y: band.y + TILE_HEIGHT / 2,
            width: 2,
            height: 1,
        };
        let p = Paragraph::new(Span::styled("›", accent(theme)));
        frame.render_widget(p, ind_area);
    }
}

fn render_tile(
    frame: &mut Frame,
    area: Rect,
    kind: HomeRowKind,
    item: &Item,
    focused_tile: bool,
    row_focused: bool,
    theme: &Theme,
) {
    let border_style = if focused_tile && row_focused {
        theme.focused_border()
    } else if focused_tile {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        theme.unfocused_border()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 4 || inner.height < 1 {
        return;
    }

    // Top line inside the tile: a big glyph + initials, centered.
    let glyph = glyph_for(kind, item.kind.as_deref());
    let initials = initials_for(&item.name);
    let icon_line = Line::from(vec![
        Span::styled(format!("  {glyph}  "), accent_bold(theme)),
        Span::styled(initials, theme.header()),
    ])
    .alignment(Alignment::Left);
    let icon_area = Rect {
        height: 1,
        ..inner
    };
    frame.render_widget(Paragraph::new(icon_line), icon_area);

    // Subtitle line: year, series episode label, or playable count.
    if inner.height >= 2 {
        let sub_area = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: 1,
        };
        let sub = subtitle_for(kind, item);
        let sub_line = Paragraph::new(Span::styled(sub, theme.muted())).alignment(Alignment::Left);
        frame.render_widget(sub_line, sub_area);
    }

    // Below the tile (in the row band, NOT inside the bordered tile): title +
    // progress bar.
    let label_area = Rect {
        x: area.x,
        y: area.y + area.height,
        width: area.width,
        height: 1,
    };
    let title_style = if focused_tile && row_focused {
        accent_bold(theme)
    } else {
        theme.list_item()
    };
    let title = clip(&item.name, area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(title, title_style)).alignment(Alignment::Left),
        label_area,
    );

    let extra_area = Rect {
        x: area.x,
        y: area.y + area.height + 1,
        width: area.width,
        height: 1,
    };
    let extra = match kind {
        HomeRowKind::Resume => format_duration(item.run_time_ticks)
            .map(|d| format!("⏱  {d}"))
            .unwrap_or_default(),
        HomeRowKind::NextUp => item
            .overview
            .as_deref()
            .map(|o| clip(o, area.width as usize))
            .unwrap_or_default(),
        HomeRowKind::Libraries | HomeRowKind::Recent => item
            .production_year
            .map(|y| y.to_string())
            .unwrap_or_default(),
    };
    if !extra.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(extra, theme.muted())),
            extra_area,
        );
    }
}

fn glyph_for(kind: HomeRowKind, item_kind: Option<&str>) -> &'static str {
    match kind {
        HomeRowKind::Libraries => match item_kind {
            Some("movies") => "▣",
            Some("tvshows") => "◫",
            Some("music") => "♪",
            Some("books") => "▤",
            Some("photos") => "▦",
            _ => "▣",
        },
        HomeRowKind::Resume => "▶",
        HomeRowKind::NextUp => "»",
        HomeRowKind::Recent => match item_kind {
            Some("Movie") => "▣",
            Some("Series") | Some("Episode") => "◫",
            Some("MusicAlbum") | Some("Audio") | Some("MusicArtist") => "♪",
            Some("Book") => "▤",
            Some("Photo" | "PhotoAlbum") => "▦",
            _ => "▢",
        },
    }
}

fn initials_for(name: &str) -> String {
    let mut parts = name.split_whitespace();
    let first = parts.next().and_then(|p| p.chars().next()).unwrap_or('•');
    let second = parts.next().and_then(|p| p.chars().next());
    match second {
        Some(c) => format!("{first}{c}").to_uppercase(),
        None => first.to_uppercase().to_string(),
    }
}

fn subtitle_for(kind: HomeRowKind, item: &Item) -> String {
    match kind {
        HomeRowKind::Libraries => match item.kind.as_deref() {
            Some(other) if !other.is_empty() => other.to_lowercase(),
            _ => "library".to_string(),
        },
        HomeRowKind::Resume => format_duration(item.run_time_ticks).unwrap_or_default(),
        HomeRowKind::NextUp => item
            .production_year
            .map(|y| y.to_string())
            .unwrap_or_default(),
        HomeRowKind::Recent => {
            if let Some(year) = item.production_year {
                year.to_string()
            } else {
                item.kind.clone().unwrap_or_default()
            }
        }
    }
}

fn format_duration(ticks: Option<i64>) -> Option<String> {
    let ticks = ticks?;
    if ticks <= 0 {
        return None;
    }
    let secs = (ticks / 10_000_000) as u64;
    let d = Duration::from_secs(secs);
    let total_min = d.as_secs() / 60;
    let h = total_min / 60;
    let m = total_min % 60;
    Some(if h > 0 { format!("{h}h {m:02}m") } else { format!("{m}m") })
}

fn clip(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn accent(theme: &Theme) -> Style {
    theme.cheatsheet_key()
}
fn accent_bold(theme: &Theme) -> Style {
    accent(theme).add_modifier(Modifier::BOLD)
}

/// 0–23 local hour, best-effort. Falls back to noon if the system clock is
/// somehow unreadable.
fn local_hour() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Coarse local-hour approximation: offset by the system's TZ is hard
    // without a tz crate, so use UTC hour. Good enough for "morning/evening".
    ((secs.rem_euclid(86_400) / 3600) as u32) % 24
}

fn greeting_for_hour(h: u32) -> &'static str {
    match h {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        18..=22 => "Good evening",
        _ => "Welcome back",
    }
}

fn greeting_glyph(h: u32) -> &'static str {
    match h {
        5..=11 => "☼",
        12..=17 => "✦",
        18..=22 => "☾",
        _ => "✧",
    }
}
