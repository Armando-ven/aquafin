//! Sidebar pane: the list of libraries.

use ratatui::layout::Rect;
use ratatui::widgets::{Block, List, ListItem, ListState};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::Library;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    libraries: &[Library],
    focused: bool,
    selected: usize,
    theme: &Theme,
) {
    let items: Vec<ListItem> = libraries
        .iter()
        .map(|library| ListItem::new(library.name.clone()))
        .collect();
    let list = List::new(items)
        .block(
            Block::bordered()
                .title(" Libraries ")
                .border_style(theme.border(focused)),
        )
        .style(theme.list_item())
        .highlight_style(theme.selected_item(focused))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}
