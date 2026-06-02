//! Library items pane (left column, top 2/3): the active drill level's items,
//! with selection markers and a breadcrumb title. Folders are flagged so it's
//! clear which entries open.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::Level;
use crate::ui::panes::item_kind_decor;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    level: Option<&Level>,
    breadcrumb: &str,
    focused: bool,
    theme: &Theme,
) {
    let title = if breadcrumb.is_empty() {
        " Items ".to_string()
    } else {
        format!(" {breadcrumb} ")
    };
    let block = Block::bordered()
        .title(title)
        .border_style(theme.border(focused));

    let Some(level) = level else {
        frame.render_widget(
            Paragraph::new("No library.").style(theme.muted()).block(block),
            area,
        );
        return;
    };

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
