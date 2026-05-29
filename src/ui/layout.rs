//! Resize-aware layout.
//!
//! Top bar (library tabs + search), then three vertical columns:
//! left and right are split 2/3 + 1/3; the middle column is bigger than either
//! side. The now-playing bar and status row sit at the bottom and are always
//! present so the rest of the UI never shifts.

use ratatui::layout::{Constraint, Layout, Rect};

/// Fixed height of the now-playing bar (1 top border + 4 content rows).
pub const NOW_PLAYING_HEIGHT: u16 = 5;
/// Top bar: borders + one content row.
pub const TOP_BAR_HEIGHT: u16 = 3;

pub struct Regions {
    pub top_bar: Rect,
    /// Left column, top 2/3: the active library's media.
    pub library_items: Rect,
    /// Left column, bottom 1/3: the library's sub-views (sections).
    pub library_sections: Rect,
    /// Middle column: content of the current selection (cover + metadata).
    pub content: Rect,
    /// Right column, top 2/3: lyrics / cast / episodes — context-specific.
    pub context_top: Rect,
    /// Right column, bottom 1/3: queue / credits / seasons — context-specific.
    pub context_bottom: Rect,
    /// The always-present now-playing bar above the status row.
    pub now_playing: Rect,
    pub status: Rect,
}

pub fn compute(area: Rect) -> Regions {
    let [top_bar, main, now_playing, status] = Layout::vertical([
        Constraint::Length(TOP_BAR_HEIGHT),
        Constraint::Min(0),
        Constraint::Length(NOW_PLAYING_HEIGHT),
        Constraint::Length(1),
    ])
    .areas(area);

    let [left, middle, right] = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(45),
        Constraint::Percentage(30),
    ])
    .areas(main);

    let [library_items, library_sections] =
        Layout::vertical([Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)]).areas(left);
    let [context_top, context_bottom] =
        Layout::vertical([Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)]).areas(right);

    Regions {
        top_bar,
        library_items,
        library_sections,
        content: middle,
        context_top,
        context_bottom,
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
