//! The F1 cheatsheet overlay: a centered popup listing the *active* bindings
//! (reflecting any `config.toml` rebindings), grouped by context.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::keymap::Keymap;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, keymap: &Keymap, theme: &Theme) {
    let mut lines: Vec<Line> = Vec::new();
    for group in keymap.describe() {
        lines.push(Line::from(Span::styled(group.title, theme.cheatsheet_group())));
        for binding in group.bindings {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<14}", binding.keys), theme.cheatsheet_key()),
                Span::raw(binding.desc),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Built-ins not in the configurable keymap: top-bar shortcuts.
    lines.push(Line::from(Span::styled("Top bar", theme.cheatsheet_group())));
    for (key, desc) in [
        ("1 – 9", "Switch library"),
        ("/", "Open search · Enter to run · Esc to cancel"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {key:<14}"), theme.cheatsheet_key()),
            Span::raw(desc),
        ]));
    }
    lines.push(Line::from(""));

    // Size the popup to its content (plus borders), clamped to the screen, so
    // adding bindings never clips the list.
    let popup = sized_center(&lines, area);
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .title(" Keybindings — press any key to close ")
        .border_style(theme.modal_border())
        .style(theme.modal());
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
