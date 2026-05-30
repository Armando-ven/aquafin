//! Content pane (middle column): the main window. When the user drills into a
//! folder (album, playlist, series, season, …) this renders its children as a
//! list; otherwise it shows the selected item's cover plus its
//! metadata/description.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme::Theme;
use crate::ui::app::{Item, Level};
use crate::ui::images::Images;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    item: Option<&Item>,
    drilled: Option<&Level>,
    focused: bool,
    images: Option<&mut Images>,
    theme: &Theme,
) {
    if let Some(level) = drilled {
        render_drilled(frame, area, level, focused, theme);
        return;
    }
    render_detail(frame, area, item, focused, images, theme);
}

fn render_drilled(frame: &mut Frame, area: Rect, level: &Level, focused: bool, theme: &Theme) {
    let block = Block::bordered()
        .title(format!(" {} ", level.title))
        .border_style(theme.border(focused));

    if level.loading {
        frame.render_widget(
            Paragraph::new("Loading…").style(theme.muted()).block(block),
            area,
        );
        return;
    }
    if level.items.is_empty() {
        frame.render_widget(
            Paragraph::new("Empty.").style(theme.muted()).block(block),
            area,
        );
        return;
    }

    let list_items: Vec<ListItem> = level
        .items
        .iter()
        .map(|item| {
            let marker = if item.is_favorite { "♥ " } else { "  " };
            let mut spans = vec![Span::raw(format!("{marker}{}", item.name))];
            if item.is_folder {
                spans.push(Span::styled("  ›", theme.folder_marker()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(list_items)
        .block(block)
        .style(theme.list_item())
        .highlight_style(theme.selected_item(focused))
        .highlight_symbol("› ");

    let mut state = ListState::default();
    state.select(Some(level.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_detail(
    frame: &mut Frame,
    area: Rect,
    item: Option<&Item>,
    focused: bool,
    images: Option<&mut Images>,
    theme: &Theme,
) {
    let block = Block::bordered()
        .title(" Content ")
        .border_style(theme.border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(item) = item else {
        frame.render_widget(
            Paragraph::new("No item selected.").style(theme.muted()),
            inner,
        );
        return;
    };

    // Reserve the top of the pane for the cover when the item has one, the
    // terminal can draw images, and there's room; text flows below it.
    let has_art = item.primary_image_tag.is_some();
    let can_draw_cover = images.as_ref().is_some_and(|im| im.is_available());
    let text_area = if has_art && can_draw_cover && inner.height >= 10 {
        let cover_height = inner.height * 3 / 5;
        let [cover_area, text_area] =
            Layout::vertical([Constraint::Length(cover_height), Constraint::Min(0)]).areas(inner);
        if let Some(images) = images {
            images.draw(frame, cover_area, &item.id);
        }
        text_area
    } else {
        inner
    };

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(item.name.clone(), theme.header()))];

    let mut meta: Vec<String> = Vec::new();
    if let Some(kind) = &item.kind {
        meta.push(kind.clone());
    }
    if let Some(year) = item.production_year {
        meta.push(year.to_string());
    }
    if let Some(runtime) = item.run_time_ticks.and_then(format_runtime) {
        meta.push(runtime);
    }
    if !meta.is_empty() {
        lines.push(Line::from(Span::styled(meta.join("  ·  "), theme.muted())));
    }

    lines.push(Line::from(""));
    match item.overview.as_deref().filter(|o| !o.is_empty()) {
        Some(overview) => lines.push(Line::from(overview.to_string())),
        None => lines.push(Line::from(Span::styled("No description.", theme.muted()))),
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), text_area);
}

/// Jellyfin `RunTimeTicks` (100 ns units) → a short `1h 56m` / `42m` string.
fn format_runtime(ticks: i64) -> Option<String> {
    if ticks <= 0 {
        return None;
    }
    let total_minutes = ticks / 10_000_000 / 60;
    let (hours, minutes) = (total_minutes / 60, total_minutes % 60);
    Some(if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    })
}
