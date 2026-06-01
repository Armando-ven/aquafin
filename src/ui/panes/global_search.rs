//! Global search view: own query input + flat result list across every
//! library. Takes over the full main band when focused. The companion `/`
//! shortcut runs a library-scoped search; this view is reached via `g s`.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::GlobalSearchState;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &GlobalSearchState,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered()
        .border_style(theme.border(focused))
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 8 || inner.height < 4 {
        return;
    }

    let [header, input, hint, results] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    render_header(frame, header, theme);
    render_input(frame, input, state, focused, theme);
    render_hint(frame, hint, state, theme);
    render_results(frame, results, state, focused, theme);
}

fn render_header(frame: &mut Frame, area: Rect, theme: &Theme) {
    let line = Line::from(vec![
        Span::styled("⌕  ", accent_bold(theme)),
        Span::styled("Global search", theme.header().add_modifier(Modifier::BOLD)),
        Span::styled("   across every library", theme.muted()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_input(
    frame: &mut Frame,
    area: Rect,
    state: &GlobalSearchState,
    view_focused: bool,
    theme: &Theme,
) {
    let prompt = Span::styled("  search › ", theme.muted());
    let typed_style = if state.input_focused && view_focused {
        theme.header()
    } else {
        theme.list_item()
    };
    let mut spans = vec![prompt, Span::styled(state.query.clone(), typed_style)];
    if state.input_focused {
        spans.push(Span::styled("█", accent_bold(theme)));
    }
    let line = Line::from(spans);
    let border_style = if state.input_focused && view_focused {
        theme.focused_border()
    } else {
        theme.unfocused_border()
    };
    let block = Block::bordered().border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(line), inner);
}

fn render_hint(frame: &mut Frame, area: Rect, state: &GlobalSearchState, theme: &Theme) {
    let text = if state.loading {
        "  Searching…".to_string()
    } else if state.input_focused {
        "  Enter to search · ↓ into results · Tab to toggle focus · Esc to leave".to_string()
    } else {
        "  ↑/↓ move · Enter open/play · ↑ at top returns to input · Esc to leave".to_string()
    };
    let line = Line::from(Span::styled(text, theme.muted().add_modifier(Modifier::DIM)));
    frame.render_widget(Paragraph::new(line), area);
}

fn render_results(
    frame: &mut Frame,
    area: Rect,
    state: &GlobalSearchState,
    view_focused: bool,
    theme: &Theme,
) {
    if state.results.is_empty() {
        let msg = if state.loading {
            "  …"
        } else if state.submitted {
            "  No matches for that query."
        } else if state.query.is_empty() {
            "  Start typing, then press Enter."
        } else {
            "  Press Enter to search."
        };
        let line = Paragraph::new(Span::styled(msg, theme.muted()));
        frame.render_widget(line, area);
        return;
    }

    let rows_visible = area.height as usize;
    let total = state.results.len();
    // Scroll so the selected row stays in view; pin to the top while focus is
    // on the input.
    let selected = state.selected.min(total.saturating_sub(1));
    let first = if state.input_focused || rows_visible == 0 {
        0
    } else {
        (selected + 1).saturating_sub(rows_visible)
    };

    let mut y = area.y;
    for (i, item) in state
        .results
        .iter()
        .enumerate()
        .skip(first)
        .take(rows_visible)
    {
        let row_area = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        let on_row = !state.input_focused && i == selected;
        let prefix = if on_row { " ▶ " } else { "   " };
        let kind_label = item.kind.as_deref().unwrap_or("");
        let kind_glyph = glyph_for(kind_label);
        let title_style = if on_row && view_focused {
            theme.selected_item(true)
        } else if on_row {
            theme.selected_item(false)
        } else {
            theme.list_item()
        };
        let meta_style = theme.muted();
        let line = Line::from(vec![
            Span::styled(prefix, accent_bold(theme)),
            Span::styled(format!("{kind_glyph}  "), meta_style),
            Span::styled(item.name.clone(), title_style),
            Span::styled(format!("   ·  {kind_label}"), meta_style),
        ]);
        frame.render_widget(Paragraph::new(line).alignment(Alignment::Left), row_area);
        y += 1;
        if y >= area.y + area.height {
            break;
        }
    }
}

fn glyph_for(kind: &str) -> &'static str {
    match kind {
        "Movie" => "▣",
        "Series" | "Episode" | "Season" => "◫",
        "Audio" | "MusicAlbum" | "MusicArtist" | "Playlist" => "♪",
        "Book" | "AudioBook" => "▤",
        "Photo" | "PhotoAlbum" => "▦",
        _ => "▢",
    }
}

fn accent_bold(theme: &Theme) -> ratatui::style::Style {
    theme.cheatsheet_key().add_modifier(Modifier::BOLD)
}
