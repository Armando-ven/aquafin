//! Content pane (middle column): reserved for actual content — a drilled-in
//! album's tracks, a series' episodes, a playlist's songs, etc. The focused
//! item's cover, title, and description live in the info pane (right column
//! top); this pane stays a placeholder until the user drills in.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::Level;

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
    let block = Block::bordered()
        .title(format!(" {} ", level.title))
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

    let list_items: Vec<ListItem> = level
        .items
        .iter()
        .map(|item| {
            let marker = if item.is_favorite { "♥ " } else { "  " };
            let mut spans = vec![Span::raw(format!("{marker}{}", item.name))];
            if item.is_folder {
                spans.push(Span::styled("  ›", theme.folder_marker()));
            }
            ListItem::new(Line::from(spans))
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
