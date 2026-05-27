//! Main list pane: the active drill level's items, with selection markers and a
//! breadcrumb title. Folders are flagged so it's clear which entries open.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::{border_style, highlight_style};
use crate::ui::app::Level;

pub fn render(frame: &mut Frame, area: Rect, level: Option<&Level>, breadcrumb: &str, focused: bool) {
    let title = if breadcrumb.is_empty() {
        " Items ".to_string()
    } else {
        format!(" {breadcrumb} ")
    };
    let block = Block::bordered()
        .title(title)
        .border_style(border_style(focused));

    let Some(level) = level else {
        frame.render_widget(Paragraph::new("No library.").block(block), area);
        return;
    };

    if level.loading {
        frame.render_widget(
            Paragraph::new(Span::styled("Loading…", Style::new().fg(Color::DarkGray))).block(block),
            area,
        );
        return;
    }
    if level.items.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("Empty.", Style::new().fg(Color::DarkGray))).block(block),
            area,
        );
        return;
    }

    let list_items: Vec<ListItem> = level
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let marker = if level.marks.contains(&index) { "● " } else { "  " };
            // A trailing arrow signals an item you can drill into.
            let mut spans = vec![Span::raw(format!("{marker}{}", item.name))];
            if item.is_folder {
                spans.push(Span::styled("  ›", Style::new().fg(Color::DarkGray)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(list_items)
        .block(block)
        .highlight_style(highlight_style(focused))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(level.selected));
    frame.render_stateful_widget(list, area, &mut state);
}
