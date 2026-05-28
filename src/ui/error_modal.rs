//! A user-facing error overlay: error summary + log location + key hints.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::layout::centered_rect;
use crate::theme::Theme;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    message: &str,
    log_location: &str,
    copied: bool,
    theme: &Theme,
) {
    let popup = centered_rect(64, 50, area);
    frame.render_widget(Clear, popup);

    let mut lines = vec![
        Line::from(Span::styled("Something went wrong", theme.error_text())),
        Line::from(""),
    ];
    for line in message.lines() {
        lines.push(Line::from(line.to_string()));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Log: {log_location}"),
        theme.muted(),
    )));
    lines.push(Line::from(""));
    let hint = if copied {
        "Enter  dismiss      y  copied!"
    } else {
        "Enter  dismiss      y  copy log path"
    };
    lines.push(Line::from(Span::styled(hint, theme.muted())));

    let block = Block::bordered()
        .title(" Error ")
        .border_style(theme.error_border())
        .style(theme.modal());
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        popup,
    );
}

/// Best-effort copy to the terminal clipboard via the OSC 52 escape sequence.
/// Works in kitty/ghostty/foot and many others; silently no-ops where unsupported.
pub(crate) fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    let sequence = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut stdout = std::io::stdout();
    stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush())
        .is_ok()
}

fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }
}
