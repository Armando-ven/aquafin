//! The three content panes (sidebar / list / detail) and their shared styling.

pub mod detail;
pub mod list;
pub mod sidebar;

use ratatui::style::{Color, Modifier, Style};

/// Border style for a pane, brighter when focused.
pub(super) fn border_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

/// Selected-row style; the focused pane gets a solid highlight, others a subtle one.
pub(super) fn highlight_style(focused: bool) -> Style {
    if focused {
        Style::new()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::REVERSED)
    }
}
