//! Centered popup menu used by the item-options and client-settings menus.
//! Up/Down moves the cursor, Enter activates, Esc closes.

use ratatui::layout::Rect;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState};
use ratatui::Frame;

use super::layout::centered_rect;
use crate::theme::Theme;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    entries: &[&str],
    selected: usize,
    theme: &Theme,
) {
    let popup = centered_rect(50, 50, area);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = entries.iter().map(|label| ListItem::new(*label)).collect();
    let block = Block::bordered()
        .title(format!(" {title} — Enter selects, Esc cancels "))
        .border_style(theme.modal_border())
        .style(theme.modal());
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected_item(true))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(selected.min(entries.len().saturating_sub(1))));
    frame.render_stateful_widget(list, popup, &mut state);
}
