//! The five content panes (top bar, library items, library sections, content,
//! context). Styling comes from the active [`crate::theme::Theme`].

pub mod content;
pub mod context_pane;
pub mod global_search;
pub mod home;
pub mod library_items;
pub mod library_sections;
pub mod top_bar;

use ratatui::style::Style;
use ratatui::text::Span;

use crate::theme::Theme;
use crate::ui::app::{Item, MediaKind};

/// Visual signal for the kind of an item in a list:
/// - playable leaves get a coloured ▶ / ♪ glyph and the regular text style,
/// - containers (album, playlist, series, season, …) get a kind-specific glyph
///   plus the bold `header` text style so the eye separates "open this" from
///   "play this" at a glance.
pub fn item_kind_decor(item: &Item, theme: &Theme) -> ItemKindDecor {
    let kind = item.kind.as_deref().unwrap_or("");
    let (glyph, glyph_style, name_style) = match kind {
        "Audio" | "AudioBook" => ("♪", theme.progress_bar(), theme.list_item()),
        "Movie" | "Episode" | "Video" | "MusicVideo" | "Trailer" => {
            ("▶", theme.progress_bar(), theme.list_item())
        }
        "MusicAlbum" => ("▣", theme.folder_marker(), theme.header()),
        "MusicArtist" => ("♫", theme.folder_marker(), theme.header()),
        "Playlist" => ("≡", theme.folder_marker(), theme.header()),
        "Series" => ("❒", theme.folder_marker(), theme.header()),
        "Season" => (
            "❍",
            theme.folder_marker(),
            theme.header().add_modifier(ratatui::style::Modifier::ITALIC),
        ),
        "BoxSet" | "Collection" | "CollectionFolder" => {
            ("❖", theme.folder_marker(), theme.header())
        }
        "Genre" | "MusicGenre" | "Studio" | "Tag" => {
            ("#", theme.folder_marker(), theme.list_item().add_modifier(ratatui::style::Modifier::ITALIC))
        }
        _ => {
            if item.is_folder {
                ("›", theme.folder_marker(), theme.header())
            } else {
                match MediaKind::classify(item.kind.as_deref()) {
                    MediaKind::Audio => ("♪", theme.progress_bar(), theme.list_item()),
                    MediaKind::Video => ("▶", theme.progress_bar(), theme.list_item()),
                    MediaKind::Other => ("·", theme.muted(), theme.list_item()),
                }
            }
        }
    };
    ItemKindDecor { glyph, glyph_style, name_style }
}

pub struct ItemKindDecor {
    pub glyph: &'static str,
    pub glyph_style: Style,
    pub name_style: Style,
}

impl ItemKindDecor {
    /// Render `[heart] [kind-glyph] [name]` for a list row.
    pub fn line_spans(&self, item: &Item) -> Vec<Span<'static>> {
        let favorite = if item.is_favorite { "♥ " } else { "" };
        vec![
            Span::raw(favorite.to_string()),
            Span::styled(format!("{} ", self.glyph), self.glyph_style),
            Span::styled(item.name.clone(), self.name_style),
        ]
    }
}
