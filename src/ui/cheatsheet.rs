//! The F1 cheatsheet overlay: a centered popup listing the *active* bindings
//! (reflecting any `config.toml` rebindings), grouped by context.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::keymap::Keymap;

pub fn render(frame: &mut Frame, area: Rect, keymap: &Keymap) {
    let mut lines: Vec<Line> = Vec::new();
    for group in keymap.describe() {
        lines.push(Line::from(Span::styled(
            group.title,
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
        for binding in group.bindings {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<14}", binding.keys), Style::new().fg(Color::Yellow)),
                Span::raw(binding.desc),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Size the popup to its content (plus borders), clamped to the screen, so
    // adding bindings never clips the list.
    let popup = sized_center(&lines, area);
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .title(" Keybindings — press any key to close ")
        .border_style(Style::new().fg(Color::Cyan));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// A rectangle centered in `area`, tall enough for `lines` (+ borders) and ~60%
/// wide, clamped so it always fits on screen.
fn sized_center(lines: &[Line], area: Rect) -> Rect {
    let height = (lines.len() as u16 + 2).min(area.height);
    let width = (area.width * 3 / 5).clamp(40.min(area.width), area.width);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}
