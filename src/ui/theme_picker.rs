//! The runtime theme picker: a centered overlay listing selectable themes.
//! Up/Down to move, Enter to apply, Esc to cancel.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState};
use ratatui::Frame;

use super::layout::centered_rect;
use crate::theme::Theme;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    names: &[String],
    selected: usize,
    active: &str,
    theme: &Theme,
) {
    let popup = centered_rect(50, 60, area);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = names
        .iter()
        .map(|name| {
            // Mark the currently-applied theme.
            let marker = if name == active { "● " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(marker, theme.now_playing_subtitle()),
                Span::raw(name.clone()),
            ]))
        })
        .collect();

    let block = Block::bordered()
        .title(" Theme — Enter applies, Esc cancels ")
        .border_style(theme.modal_border())
        .style(theme.modal());
    let list = List::new(items)
        .block(block)
        .highlight_style(theme.selected_item(true))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(selected.min(names.len().saturating_sub(1))));
    frame.render_stateful_widget(list, popup, &mut state);
}
