//! The now-playing bar: a fixed-height strip above the status bar. It's always
//! present (an idle placeholder when nothing plays) so the rest of the UI never
//! shifts when playback starts or stops. While playing it shows the cover, the
//! title/subtitle, a progress gauge, and elapsed/total time (+ volume for audio).

use std::time::Duration;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, LineGauge, Paragraph};
use ratatui::Frame;

use super::images::Images;
use crate::theme::Theme;
use crate::ui::app::{MediaKind, NowPlaying};

pub fn render(
    frame: &mut Frame,
    area: Rect,
    now_playing: Option<&NowPlaying>,
    images: Option<&mut Images>,
    theme: &Theme,
) {
    // A top border separates the bar from the panes above.
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme.unfocused_border());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(np) = now_playing else {
        frame.render_widget(
            Paragraph::new("Nothing playing")
                .style(theme.muted())
                .alignment(Alignment::Center),
            inner,
        );
        return;
    };

    // Only reserve cover space when the terminal can actually draw images, so
    // non-graphical terminals don't get an empty gap.
    let can_draw_cover = images.as_ref().is_some_and(|im| im.is_available());
    let info = if can_draw_cover {
        // Square-ish cover on the left (cells are ~half as wide as tall).
        let cover_width = (inner.height * 2).min(inner.width / 3);
        let [cover_area, _gap, info] = Layout::horizontal([
            Constraint::Length(cover_width),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);
        if let Some(images) = images {
            images.draw(frame, cover_area, &np.item_id);
        }
        info
    } else {
        inner
    };

    render_info(frame, info, np, theme);
}

fn render_info(frame: &mut Frame, area: Rect, np: &NowPlaying, theme: &Theme) {
    let [title_row, subtitle_row, gauge_row, meta_row] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // Title, with an ASCII state marker (no emoji — avoids width glitches).
    let marker = match np.kind {
        MediaKind::Video => ">",
        _ if np.paused => "||",
        _ => ">",
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{marker} "), theme.focused_border()),
            Span::styled(np.title.clone(), theme.now_playing_title()),
        ])),
        title_row,
    );

    if let Some(subtitle) = &np.subtitle {
        frame.render_widget(
            Paragraph::new(Span::styled(subtitle.clone(), theme.now_playing_subtitle())),
            subtitle_row,
        );
    }

    // Progress gauge filling the row width.
    let ratio = match np.duration {
        Some(total) if total.as_secs_f64() > 0.0 => {
            (np.position.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };
    frame.render_widget(
        LineGauge::default()
            .ratio(ratio)
            .filled_style(theme.progress_bar())
            .unfilled_style(theme.progress_track()),
        gauge_row,
    );

    // Time on the left, state/volume on the right.
    let times = format!(
        "{} / {}",
        format_time(np.position),
        np.duration.map(format_time).unwrap_or_else(|| "--:--".into()),
    );
    let right = match np.kind {
        MediaKind::Video => "playing in mpv".to_string(),
        _ => {
            let state = if np.paused { "paused" } else { "playing" };
            match np.volume {
                Some(v) => format!("{state} · vol {v}%"),
                None => state.to_string(),
            }
        }
    };
    let [time_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right.chars().count() as u16)])
            .areas(meta_row);
    frame.render_widget(
        Paragraph::new(times).style(theme.now_playing_meta()),
        time_area,
    );
    frame.render_widget(
        Paragraph::new(right)
            .style(theme.now_playing_meta())
            .alignment(Alignment::Right),
        right_area,
    );
}

/// `m:ss`, or `h:mm:ss` once past an hour.
fn format_time(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time_with_and_without_hours() {
        assert_eq!(format_time(Duration::from_secs(0)), "0:00");
        assert_eq!(format_time(Duration::from_secs(83)), "1:23");
        assert_eq!(format_time(Duration::from_secs(3 * 3600 + 4 * 60 + 5)), "3:04:05");
    }
}
