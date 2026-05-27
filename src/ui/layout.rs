//! Resize-aware layout: three vertical panes over a one-row status bar, plus a
//! centered-rectangle helper for overlays.

use ratatui::layout::{Constraint, Layout, Rect};

/// Fixed height of the now-playing bar (1 top border + 4 content rows). It's
/// always present (showing an idle placeholder when nothing plays) so the rest
/// of the UI never shifts when playback starts or stops.
pub const NOW_PLAYING_HEIGHT: u16 = 5;

pub struct Regions {
    pub sidebar: Rect,
    pub list: Rect,
    pub detail: Rect,
    /// The always-present now-playing bar above the status row.
    pub now_playing: Rect,
    pub status: Rect,
}

pub fn compute(area: Rect) -> Regions {
    let [main, now_playing, status] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(NOW_PLAYING_HEIGHT),
        Constraint::Length(1),
    ])
    .areas(area);
    let [sidebar, list, detail] = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(40),
        Constraint::Percentage(35),
    ])
    .areas(main);
    Regions {
        sidebar,
        list,
        detail,
        now_playing,
        status,
    }
}

/// A rectangle centered within `area`, sized as a percentage of it.
pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let [_, vertical, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);
    let [_, center, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(vertical);
    center
}
