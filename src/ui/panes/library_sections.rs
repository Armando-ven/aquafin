//! Library sections pane (left column, bottom 1/3). Lists the static
//! [`Section`]s for the active library kind (music → Albums, Album Artists,
//! Songs…). Enter on a section refetches the items pane with that filter.

use ratatui::layout::Rect;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::Section;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    sections: &[Section],
    selected: usize,
    focused: bool,
    theme: &Theme,
) {
    let title = match sections.get(selected) {
        Some(section) => format!(" Sections · {} ", section.name),
        None => " Sections ".to_string(),
    };
    let block = Block::bordered()
        .title(title)
        .border_style(theme.border(focused));

    if sections.is_empty() {
        frame.render_widget(
            Paragraph::new("No sections.").style(theme.muted()).block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = sections
        .iter()
        .map(|section| ListItem::new(section.name.clone()))
        .collect();
    let list = List::new(items)
        .block(block)
        .style(theme.list_item())
        .highlight_style(theme.selected_item(focused))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(selected.min(sections.len() - 1)));
    frame.render_stateful_widget(list, area, &mut state);
}
