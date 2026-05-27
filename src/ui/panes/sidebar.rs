//! Sidebar pane: the list of libraries.

use ratatui::layout::Rect;
use ratatui::widgets::{Block, List, ListItem, ListState};
use ratatui::Frame;

use super::{border_style, highlight_style};
use crate::ui::app::Library;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    libraries: &[Library],
    focused: bool,
    selected: usize,
) {
    let items: Vec<ListItem> = libraries
        .iter()
        .map(|library| ListItem::new(library.name.clone()))
        .collect();
    let list = List::new(items)
        .block(
            Block::bordered()
                .title(" Libraries ")
                .border_style(border_style(focused)),
        )
        .highlight_style(highlight_style(focused))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}
