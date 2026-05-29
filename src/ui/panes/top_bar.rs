//! Top bar: library tabs (each with a 1-9 keybind chip) separated by floating
//! dots, plus a search box on the right. Library selection happens via the
//! numeric chord; the search box accepts input when `search_query` is `Some`.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::Library;

/// Width reserved for the search box on the right of the top bar.
const SEARCH_WIDTH: u16 = 32;
/// Floating-dot separator between library chips.
const DOT: &str = "  ·  ";

pub fn render(
    frame: &mut Frame,
    area: Rect,
    libraries: &[Library],
    selected: usize,
    search_query: Option<&str>,
    focused: bool,
    theme: &Theme,
) {
    let block = Block::bordered().border_style(theme.border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let [tabs_area, search_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(SEARCH_WIDTH.min(inner.width)),
    ])
    .areas(inner);

    render_tabs(frame, tabs_area, libraries, selected, theme);
    render_search(frame, search_area, search_query, theme);
}

fn render_tabs(
    frame: &mut Frame,
    area: Rect,
    libraries: &[Library],
    selected: usize,
    theme: &Theme,
) {
    let mut spans: Vec<Span> = Vec::with_capacity(libraries.len() * 4);
    for (index, library) in libraries.iter().enumerate().take(9) {
        if index > 0 {
            spans.push(Span::styled(DOT, theme.muted()));
        }
        // The numeric chip ("1", "2", …) doubles as the keybind hint.
        spans.push(Span::styled(
            format!(" {} ", index + 1),
            theme.folder_marker(),
        ));
        spans.push(Span::raw(" "));
        let name_style = if index == selected {
            theme.selected_item(true)
        } else {
            theme.list_item()
        };
        spans.push(Span::styled(library.name.clone(), name_style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_search(
    frame: &mut Frame,
    area: Rect,
    search_query: Option<&str>,
    theme: &Theme,
) {
    let line = match search_query {
        // Active search: show the in-progress query with a cursor block.
        Some(query) => Line::from(vec![
            Span::styled("/ ", theme.header()),
            Span::styled(query.to_string(), theme.list_item()),
            Span::styled("█", theme.header()),
        ]),
        None => Line::from(vec![
            Span::styled("/ ", theme.muted()),
            Span::styled("Search…", theme.muted()),
        ]),
    };
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Right), area);
}
