//! The now-playing bar: a fixed-height strip above the status bar. It's always
//! present (an idle placeholder when nothing plays) so the rest of the UI never
//! shifts when playback starts or stops. While playing it shows the cover, the
//! title / artist / album, a progress gauge, time + percent, an audio format
//! summary, and a play/pause icon.

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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.unfocused_border())
        .title(Span::styled(" Now playing ", theme.now_playing_title()));
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
        // Cover takes the full inner height (sits adjacent to the top + bottom
        // borders without overlapping them) so the image renders as large as
        // possible. Width keeps the image square at the terminal's ~2:1 cell
        // aspect — `inner.height * 2`.
        let cover_w = (inner.height * 2).min(inner.width / 2);
        let [_, cover_area, _gap, info, _] = Layout::horizontal([
            Constraint::Length(1),
            Constraint::Length(cover_w),
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .areas(inner);
        if let Some(images) = images {
            images.draw(frame, cover_area, &np.item_id);
        }
        info
    } else {
        // Without a cover, give the text rows symmetric horizontal padding.
        let [_, body, _] = Layout::horizontal([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .areas(inner);
        body
    };

    render_info(frame, info, np, theme);
}

fn render_info(frame: &mut Frame, area: Rect, np: &NowPlaying, theme: &Theme) {
    // Four content rows centered inside the 5-row inner band — half a row of
    // breathing space ends up on the bottom (the remainder is consumed by the
    // `Min(0)` slot at the end).
    let [title_row, artist_row, gauge_row, meta_row, _] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    // Title row: play/pause icon + title (full width — times live next to the
    // seek bar instead).
    let marker = match np.kind {
        MediaKind::Video => "▶",
        _ if np.paused => "⏸",
        _ => "▶",
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{marker}   "), theme.focused_border()),
            Span::styled(np.title.clone(), theme.now_playing_title()),
        ])),
        title_row,
    );

    // Artist · Album row (audio); for video, fall back to the existing subtitle.
    let artist_album_line = match np.kind {
        MediaKind::Audio => artist_album_text(np),
        _ => np.subtitle.clone(),
    };
    if let Some(text) = artist_album_line {
        frame.render_widget(
            Paragraph::new(Span::styled(text, theme.now_playing_subtitle())),
            artist_row,
        );
    }

    // Gauge row: elapsed/total on the left, the bar (with percent label) on
    // the right.
    let ratio = match np.duration {
        Some(total) if total.as_secs_f64() > 0.0 => {
            (np.position.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };
    let times = format_times(np);
    let times_width = times.chars().count() as u16;
    let [time_area, _gap, bar_area] = Layout::horizontal([
        Constraint::Length(times_width),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(gauge_row);
    frame.render_widget(
        Paragraph::new(times).style(theme.now_playing_meta()),
        time_area,
    );
    let percent = (ratio * 100.0).round() as u32;
    frame.render_widget(
        LineGauge::default()
            .ratio(ratio)
            .label(Span::styled(format!("{percent}%"), theme.now_playing_meta()))
            .filled_style(theme.progress_bar())
            .unfilled_style(theme.progress_track()),
        bar_area,
    );

    // Bottom meta row: format summary on the left, state (+ volume) on the right.
    let left = format_summary_text(np);
    let right = state_text(np);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right.chars().count() as u16)])
            .areas(meta_row);
    frame.render_widget(
        Paragraph::new(left).style(theme.now_playing_meta()),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(right)
            .style(theme.now_playing_meta())
            .alignment(Alignment::Right),
        right_area,
    );
}

/// `m:ss / m:ss` when the duration is known; `m:ss / --:--` otherwise.
/// The seek bar carries the percent label, so it isn't duplicated here.
fn format_times(np: &NowPlaying) -> String {
    let pos = format_time(np.position);
    match np.duration {
        Some(total) if total.as_secs_f64() > 0.0 => {
            format!("{} / {}", pos, format_time(total))
        }
        _ => format!("{pos} / --:--"),
    }
}

/// `"Artist · Album"`, falling back to whichever side is present (or `None`).
fn artist_album_text(np: &NowPlaying) -> Option<String> {
    match (np.artist.as_deref(), np.album.as_deref()) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => Some(format!("{a} · {b}")),
        (Some(a), _) if !a.is_empty() => Some(a.to_string()),
        (_, Some(b)) if !b.is_empty() => Some(b.to_string()),
        _ => np.subtitle.clone(),
    }
}

fn format_summary_text(np: &NowPlaying) -> String {
    np.format_summary.clone().unwrap_or_default()
}

fn state_text(np: &NowPlaying) -> String {
    match np.kind {
        MediaKind::Video => "playing in mpv".to_string(),
        _ => {
            let state = if np.paused { "paused" } else { "playing" };
            match np.volume {
                Some(v) => format!("{state} · vol {v}%"),
                None => state.to_string(),
            }
        }
    }
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
    use crate::ui::app::MediaKind;

    fn np_audio() -> NowPlaying {
        NowPlaying {
            item_id: "x".to_string(),
            kind: MediaKind::Audio,
            title: "Get Lucky".to_string(),
            subtitle: None,
            artist: Some("Daft Punk".to_string()),
            album: Some("Random Access Memories".to_string()),
            format_summary: Some("FLAC · 44.1 kHz · stereo · 1466 kbps".to_string()),
            position: Duration::from_secs(30),
            duration: Some(Duration::from_secs(200)),
            paused: false,
            volume: Some(80),
        }
    }

    #[test]
    fn formats_time_with_and_without_hours() {
        assert_eq!(format_time(Duration::from_secs(0)), "0:00");
        assert_eq!(format_time(Duration::from_secs(83)), "1:23");
        assert_eq!(format_time(Duration::from_secs(3 * 3600 + 4 * 60 + 5)), "3:04:05");
    }

    #[test]
    fn format_times_is_just_elapsed_over_total() {
        let np = np_audio();
        assert_eq!(format_times(&np), "0:30 / 3:20");
    }

    #[test]
    fn format_times_falls_back_when_duration_unknown() {
        let mut np = np_audio();
        np.duration = None;
        assert_eq!(format_times(&np), "0:30 / --:--");
    }

    #[test]
    fn artist_album_joins_with_separator() {
        let np = np_audio();
        assert_eq!(
            artist_album_text(&np).as_deref(),
            Some("Daft Punk · Random Access Memories")
        );
    }

    #[test]
    fn artist_album_falls_back_to_whichever_side_is_set() {
        let mut np = np_audio();
        np.album = None;
        assert_eq!(artist_album_text(&np).as_deref(), Some("Daft Punk"));
        np.artist = None;
        np.album = Some("RAM".to_string());
        assert_eq!(artist_album_text(&np).as_deref(), Some("RAM"));
    }
}
