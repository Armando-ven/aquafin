//! Serde data models for the Jellyfin API (responses + request bodies) and the
//! persisted credentials struct.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// --- Authentication ---------------------------------------------------------

/// Response from `AuthenticateByName` / `AuthenticateWithQuickConnect`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticationResult {
    pub user: User,
    pub access_token: String,
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct User {
    pub id: String,
    pub name: Option<String>,
}

/// State of a Quick Connect request.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct QuickConnectResult {
    pub authenticated: bool,
    pub secret: String,
    pub code: String,
}

/// JSON body for `POST /Users/AuthenticateByName`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticateByNameRequest<'a> {
    pub username: &'a str,
    pub pw: &'a str,
}

/// JSON body for `POST /Users/AuthenticateWithQuickConnect`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct QuickConnectAuthRequest<'a> {
    pub secret: &'a str,
}

/// Credentials persisted to `credentials.toml` (snake_case TOML keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub server_url: String,
    pub user_id: String,
    pub token: String,
    pub device_id: String,
}

// --- System -----------------------------------------------------------------

/// Unauthenticated server info from `GET /System/Info/Public`; used to validate
/// a server URL during setup.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct PublicSystemInfo {
    pub server_name: Option<String>,
    pub version: Option<String>,
    pub id: Option<String>,
    pub product_name: Option<String>,
}

// --- Items ------------------------------------------------------------------

/// A library item. This maps a useful subset of Jellyfin's `BaseItemDto`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct BaseItemDto {
    pub id: String,
    pub name: Option<String>,
    #[serde(rename = "Type")]
    pub type_: Option<String>,
    pub server_id: Option<String>,
    pub is_folder: Option<bool>,
    pub collection_type: Option<String>,
    pub overview: Option<String>,
    pub production_year: Option<i32>,
    pub run_time_ticks: Option<i64>,
    pub index_number: Option<i32>,
    pub parent_index_number: Option<i32>,
    pub series_name: Option<String>,
    pub season_name: Option<String>,
    pub parent_id: Option<String>,
    pub primary_image_aspect_ratio: Option<f64>,
    pub user_data: Option<UserData>,
    pub image_tags: Option<HashMap<String, String>>,
    pub backdrop_image_tags: Option<Vec<String>>,
    /// Cast and crew. Populated when `fields=People` is requested or when the
    /// detail endpoint (`/Items/{id}`) is used.
    pub people: Option<Vec<BaseItemPerson>>,
    pub genres: Option<Vec<String>>,
    /// External trailer links (e.g. YouTube). Populated for Movies/Series when
    /// the server has any.
    pub remote_trailers: Option<Vec<MediaUrl>>,
    pub local_trailer_count: Option<i32>,
    /// Chapter markers (movies, some episodes). Populated when `fields=Chapters`
    /// is requested.
    pub chapters: Option<Vec<ChapterInfo>>,
    /// ReplayGain-style per-track normalization in dB (negative ⇒ attenuate).
    /// Populated by Jellyfin when the source carries a `REPLAYGAIN_TRACK_GAIN` tag.
    pub normalization_gain: Option<f32>,
    /// Parent album id for an `Audio` track. Drives the "Go to album" jump.
    pub album_id: Option<String>,
    /// Album name (display only — the id lives in `album_id`).
    pub album: Option<String>,
    /// Album artists with ids; populated on `MusicAlbum` and `Audio` items.
    /// Drives the "Go to artist" jump.
    pub album_artists: Vec<NameGuidPair>,
    /// Track artists with ids (`Audio` / `MusicVideo`). Used as a fallback when
    /// `album_artists` is empty.
    pub artist_items: Vec<NameGuidPair>,
}

/// Jellyfin `NameGuidPair` — a `{Name, Id}` reference returned in fields like
/// `AlbumArtists` and `ArtistItems`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct NameGuidPair {
    pub name: Option<String>,
    pub id: Option<String>,
}

/// One chapter marker on an item (a name + a timestamp into the file).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ChapterInfo {
    pub name: Option<String>,
    /// Start position, in Jellyfin 100 ns ticks.
    pub start_position_ticks: Option<i64>,
    pub image_tag: Option<String>,
}

/// A `{Url, Name}` pair from Jellyfin's `RemoteTrailers` field.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaUrl {
    pub url: Option<String>,
    pub name: Option<String>,
}

/// A cast or crew member from a `BaseItemDto.People` entry.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct BaseItemPerson {
    pub id: Option<String>,
    pub name: Option<String>,
    /// e.g. `Actor`, `Director`, `Writer`, `GuestStar`.
    #[serde(rename = "Type")]
    pub type_: Option<String>,
    pub role: Option<String>,
}

/// `GET /Items/{itemId}/PlaybackInfo` response (subset). Carries the list of
/// playable media sources and their embedded streams (audio + subtitle).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct PlaybackInfoResponse {
    pub media_sources: Vec<MediaSource>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSource {
    pub id: Option<String>,
    pub name: Option<String>,
    pub container: Option<String>,
    pub size: Option<i64>,
    pub run_time_ticks: Option<i64>,
    pub media_streams: Vec<MediaStream>,
}

/// One embedded track inside a [`MediaSource`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaStream {
    #[serde(rename = "Type")]
    pub type_: Option<String>,
    /// Absolute stream index inside the file (across all types).
    pub index: i32,
    pub language: Option<String>,
    pub title: Option<String>,
    pub display_title: Option<String>,
    pub codec: Option<String>,
    pub channel_layout: Option<String>,
    pub is_default: Option<bool>,
    pub is_forced: Option<bool>,
}

/// `GET /Items/{itemId}/Lyrics` response.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LyricsDto {
    pub lyrics: Vec<LyricLine>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct LyricLine {
    pub text: String,
    /// Optional timecode in 100 ns ticks; absent on plain-text lyrics.
    pub start: Option<i64>,
}

/// Per-user playback state attached to an item.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserData {
    pub playback_position_ticks: Option<i64>,
    pub play_count: Option<i32>,
    pub played: Option<bool>,
    pub played_percentage: Option<f64>,
    pub is_favorite: Option<bool>,
}

/// Paged result wrapper used by most item-listing endpoints.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ItemsResult {
    pub items: Vec<BaseItemDto>,
    pub total_record_count: i64,
    pub start_index: i64,
}

/// Query parameters for `GET /Users/{userId}/Items`.
#[derive(Debug, Clone, Default)]
pub struct ItemsQuery {
    pub parent_id: Option<String>,
    pub search_term: Option<String>,
    pub include_item_types: Vec<String>,
    /// Extra fields to populate (e.g. `Overview`), which Jellyfin omits by default.
    pub fields: Vec<String>,
    /// Sort keys (e.g. `ParentIndexNumber`, `IndexNumber`, `SortName`).
    pub sort_by: Vec<String>,
    pub recursive: Option<bool>,
    pub start_index: Option<u32>,
    pub limit: Option<u32>,
    /// Filter to items whose AlbumArtist matches one of these ids. Used to
    /// fetch the albums credited to an artist.
    pub album_artist_ids: Vec<String>,
    /// Filter to items whose contributing artists include one of these ids.
    /// Used for an artist's "appears on" list.
    pub artist_ids: Vec<String>,
    /// Filter to items tagged with these genres (Jellyfin `genres` query param).
    pub genres: Vec<String>,
    /// Filter to items credited to one of these people (`personIds`). Used by
    /// the "browse by person" / filmography flow.
    pub person_ids: Vec<String>,
    /// Filter to items whose studio matches one of these (`studioIds`).
    pub studio_ids: Vec<String>,
    /// Filter to items released in one of these years (`years` query param).
    pub years: Vec<i32>,
    /// Filter to items tagged with one of these tags (`tags`).
    pub tags: Vec<String>,
    /// Jellyfin `filters` query, comma-joined (e.g. `IsUnplayed,IsFavorite`).
    pub filters: Vec<String>,
}

impl ItemsQuery {
    /// Flatten into the (key, value) query pairs Jellyfin expects, omitting unset fields.
    pub fn to_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(v) = &self.parent_id {
            pairs.push(("parentId", v.clone()));
        }
        if let Some(v) = &self.search_term {
            pairs.push(("searchTerm", v.clone()));
        }
        if !self.include_item_types.is_empty() {
            pairs.push(("includeItemTypes", self.include_item_types.join(",")));
        }
        if !self.fields.is_empty() {
            pairs.push(("fields", self.fields.join(",")));
        }
        if !self.sort_by.is_empty() {
            pairs.push(("sortBy", self.sort_by.join(",")));
        }
        if let Some(v) = self.recursive {
            pairs.push(("recursive", v.to_string()));
        }
        if let Some(v) = self.start_index {
            pairs.push(("startIndex", v.to_string()));
        }
        if let Some(v) = self.limit {
            pairs.push(("limit", v.to_string()));
        }
        if !self.album_artist_ids.is_empty() {
            pairs.push(("albumArtistIds", self.album_artist_ids.join(",")));
        }
        if !self.artist_ids.is_empty() {
            pairs.push(("artistIds", self.artist_ids.join(",")));
        }
        if !self.genres.is_empty() {
            pairs.push(("genres", self.genres.join("|")));
        }
        if !self.person_ids.is_empty() {
            pairs.push(("personIds", self.person_ids.join(",")));
        }
        if !self.studio_ids.is_empty() {
            pairs.push(("studioIds", self.studio_ids.join(",")));
        }
        if !self.years.is_empty() {
            pairs.push((
                "years",
                self.years.iter().map(|y| y.to_string()).collect::<Vec<_>>().join(","),
            ));
        }
        if !self.tags.is_empty() {
            pairs.push(("tags", self.tags.join("|")));
        }
        if !self.filters.is_empty() {
            pairs.push(("filters", self.filters.join(",")));
        }
        pairs
    }
}

// --- Search -----------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SearchHintResult {
    pub search_hints: Vec<SearchHint>,
    pub total_record_count: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SearchHint {
    pub item_id: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    #[serde(rename = "Type")]
    pub type_: Option<String>,
}

// --- Images -----------------------------------------------------------------

/// Raw image bytes plus the response content type.
#[derive(Debug, Clone)]
pub struct ImageResponse {
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

// --- Playback reporting -----------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackStartInfo {
    pub item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ticks: Option<i64>,
    pub is_paused: bool,
    pub can_seek: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_source_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackProgressInfo {
    pub item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ticks: Option<i64>,
    pub is_paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_level: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackStopInfo {
    pub item_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ticks: Option<i64>,
}
