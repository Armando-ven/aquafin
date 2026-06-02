//! On-demand library queries: folder drilling, section filters, and search.
//!
//! All fetches run on the async runtime; results come back to the UI thread
//! over a channel and fill the matching loading level. The UI thread never
//! blocks.

use std::sync::mpsc::{self, Receiver, Sender};

use tokio::runtime::Handle;

use crate::api::models::ItemsQuery;
use crate::api::JellyfinClient;

use super::app::{
    App, HomeData, HomeLibrarySummary, Intent, Item, Library, MediaKind, MediaVersion, Section,
    SectionFilters,
    TrackPickerEntry, VideoTrackKind,
};
use crate::video::TrackChoice;

/// Result of an async fetch, tagged so the UI knows which level to fill.
enum BrowseResult {
    Folder {
        id: String,
        items: Vec<Item>,
    },
    Section {
        library_id: String,
        title: String,
        items: Vec<Item>,
    },
    Search {
        library_id: String,
        query: String,
        items: Vec<Item>,
    },
    /// Server-wide search results — fed into the global-search view.
    GlobalSearch {
        query: String,
        items: Vec<Item>,
    },
    Failed {
        id: String,
        message: String,
    },
    /// Outcome of a `SyncLibraries` request: the refreshed (already-filtered)
    /// library list.
    Libraries(Vec<Library>),
    /// Server-side library list for the visible-libraries picker; each entry
    /// is (id, name, currently-visible?).
    LibraryMeta(Vec<(String, String, bool)>),
    /// The user's playlists, fetched for the playlist picker.
    Playlists(Vec<(String, String)>),
    /// Mix tracks fetched for `Intent::InstantMix`. The controller queues them
    /// and starts playback of the first track.
    InstantMix(Vec<Item>),
    /// Audio / subtitle tracks for the open video track picker.
    VideoTracks(Vec<TrackPickerEntry>),
    /// Versions + audio + subtitle entries for the media-options view.
    MediaOptions {
        item_id: String,
        versions: Vec<MediaVersion>,
        audio_entries: Vec<TrackPickerEntry>,
        subtitle_entries: Vec<TrackPickerEntry>,
    },
    /// Server action finished — show this as a status-bar note.
    Status(String),
    /// Server action failed — surface via the error modal.
    Error(String),
    /// Home dashboard payload (resume + next-up + recents per library).
    Home(HomeData),
}

pub struct Browser {
    rt: Handle,
    client: JellyfinClient,
    tx: Sender<BrowseResult>,
    rx: Receiver<BrowseResult>,
}

impl Browser {
    pub fn new(rt: Handle, client: JellyfinClient) -> Self {
        let (tx, rx) = mpsc::channel();
        Self { rt, client, tx, rx }
    }

    /// Begin loading the children of folder `id`. The loading level was already
    /// pushed by the UI; [`Browser::tick`] fills it when the fetch returns.
    pub fn open(&mut self, id: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let query = ItemsQuery {
                parent_id: Some(id.clone()),
                fields: vec!["Overview".to_string()],
                // Index order keeps episodes and album tracks in sequence;
                // SortName is the sensible fallback for seasons/albums.
                sort_by: vec![
                    "ParentIndexNumber".to_string(),
                    "IndexNumber".to_string(),
                    "SortName".to_string(),
                ],
                limit: Some(500),
                ..Default::default()
            };
            let result = match client.items(&query).await {
                Ok(items) => BrowseResult::Folder {
                    id,
                    items: items.items.into_iter().map(super::item_from_dto).collect(),
                },
                Err(e) => BrowseResult::Failed {
                    id,
                    message: format!("Couldn't open folder: {e}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    /// Refetch the library's root level with a section filter applied.
    pub fn apply_section(
        &mut self,
        library_id: String,
        library_name: String,
        section: Section,
        extras: SectionFilters,
    ) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let title = format!("{library_name} · {}", section.name);
        self.rt.spawn(async move {
            let result = match fetch_section_items(&client, &library_id, &section, &extras).await {
                Ok(items) => BrowseResult::Section {
                    library_id,
                    title,
                    items,
                },
                Err(e) => BrowseResult::Failed {
                    id: library_id,
                    message: format!("Couldn't load section: {e}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    /// Library-scoped search: runs an `Items` query with `parentId` +
    /// `searchTerm` (recursive) so results stay within the current library.
    /// Results replace the active library's root level.
    pub fn search(&mut self, library_id: String, query: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let q = ItemsQuery {
                parent_id: Some(library_id.clone()),
                search_term: Some(query.clone()),
                recursive: Some(true),
                sort_by: vec!["SortName".to_string()],
                fields: vec!["Overview".to_string()],
                limit: Some(200),
                ..Default::default()
            };
            let result = match client.items(&q).await {
                Ok(list) => {
                    let items = list.items.into_iter().map(super::item_from_dto).collect();
                    BrowseResult::Search {
                        library_id,
                        query,
                        items,
                    }
                }
                Err(e) => BrowseResult::Failed {
                    id: library_id,
                    message: format!("Couldn't search: {e}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    /// Server-wide search (the `g s` view). Uses `/Search/Hints` so every
    /// library is covered in one round trip.
    pub fn global_search(&mut self, query: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = match client.search_hints(&query).await {
                Ok(hints) => {
                    let items = hints
                        .search_hints
                        .into_iter()
                        .filter_map(item_from_hint)
                        .collect();
                    BrowseResult::GlobalSearch { query, items }
                }
                Err(e) => BrowseResult::Error(format!("Search failed: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Re-fetch every library + its top-level items. Used by the
    /// "Sync with server now" client-menu action.
    pub fn sync_libraries(&mut self, visible: Option<Vec<String>>) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = match fetch_libraries(&client, visible.as_deref()).await {
                Ok(libraries) => BrowseResult::Libraries(libraries),
                Err(e) => BrowseResult::Error(format!("Sync failed: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Fetch the full server-side library list (including currently-hidden
    /// ones) for the visible-libraries picker. Each entry is tagged with its
    /// current visibility based on the `visible` list (None = all visible).
    pub fn load_library_meta(&mut self, visible: Option<Vec<String>>) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = match client.user_views().await {
                Ok(views) => {
                    let entries = views
                        .into_iter()
                        .map(|v| {
                            let name = v.name.unwrap_or_else(|| "(library)".to_string());
                            let on = match &visible {
                                None => true,
                                Some(list) => list.iter().any(|id| id == &v.id),
                            };
                            (v.id, name, on)
                        })
                        .collect();
                    BrowseResult::LibraryMeta(entries)
                }
                Err(e) => BrowseResult::Error(format!("Couldn't fetch libraries: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Fetch the user's playlists for the Add-to-playlist picker.
    pub fn load_playlists(&mut self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let query = ItemsQuery {
                include_item_types: vec!["Playlist".to_string()],
                recursive: Some(true),
                sort_by: vec!["SortName".to_string()],
                limit: Some(500),
                ..Default::default()
            };
            let result = match client.items(&query).await {
                Ok(list) => BrowseResult::Playlists(
                    list.items
                        .into_iter()
                        .map(|dto| (dto.id, dto.name.unwrap_or_else(|| "(playlist)".to_string())))
                        .collect(),
                ),
                Err(e) => BrowseResult::Error(format!("Couldn't fetch playlists: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// `POST /Playlists/{playlistId}/Items` — append one item.
    pub fn add_to_playlist(&mut self, playlist_id: String, item_id: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = match client.add_to_playlist(&playlist_id, &[item_id.as_str()]).await {
                Ok(()) => BrowseResult::Status("Added to playlist".to_string()),
                Err(e) => BrowseResult::Error(format!("Add to playlist failed: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Build a "genre radio" queue: pull a random batch of audio items
    /// matching `genre` (recursive in `library_id`).
    pub fn genre_radio(&mut self, library_id: String, genre: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let q = crate::api::models::ItemsQuery {
                parent_id: Some(library_id),
                include_item_types: vec!["Audio".to_string()],
                genres: vec![genre],
                sort_by: vec!["Random".to_string()],
                recursive: Some(true),
                limit: Some(50),
                fields: vec!["Overview".to_string()],
                ..Default::default()
            };
            let result = match client.items(&q).await {
                Ok(list) => {
                    let items: Vec<Item> = list
                        .items
                        .into_iter()
                        .map(super::item_from_dto)
                        .filter(|i| {
                            matches!(MediaKind::classify(i.kind.as_deref()), MediaKind::Audio)
                        })
                        .collect();
                    if items.is_empty() {
                        BrowseResult::Error("Genre radio returned no tracks".to_string())
                    } else {
                        BrowseResult::InstantMix(items)
                    }
                }
                Err(e) => BrowseResult::Error(format!("Genre radio failed: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Create a server-side playlist named `name` with the supplied items.
    pub fn create_playlist(&mut self, name: String, item_ids: Vec<String>) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = match client.create_playlist(&name, &item_ids).await {
                Ok(id) => BrowseResult::Status(format!("Saved as playlist \"{name}\" ({id})")),
                Err(e) => BrowseResult::Error(format!("Couldn't save playlist: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Fetch the Instant Mix queue for `item`.
    pub fn instant_mix(&mut self, item: Item) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = match client.instant_mix(&item.id, 50).await {
                Ok(list) => {
                    let items: Vec<Item> = list
                        .items
                        .into_iter()
                        .map(super::item_from_dto)
                        .filter(|i| matches!(MediaKind::classify(i.kind.as_deref()), MediaKind::Audio))
                        .collect();
                    if items.is_empty() {
                        BrowseResult::Error("Instant mix returned no playable tracks".to_string())
                    } else {
                        BrowseResult::InstantMix(items)
                    }
                }
                Err(e) => BrowseResult::Error(format!("Instant mix failed: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Fetch `PlaybackInfo` and populate the media-options view (versions +
    /// audio + subtitle entries) in a single round-trip.
    pub fn load_media_options(&mut self, item_id: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = match client.playback_info(&item_id).await {
                Ok(info) => {
                    let versions = info
                        .media_sources
                        .iter()
                        .filter_map(|s| {
                            s.id.clone().map(|id| MediaVersion {
                                label: format_version_label(s),
                                source_id: id,
                            })
                        })
                        .collect();
                    let streams = info
                        .media_sources
                        .into_iter()
                        .next()
                        .map(|s| s.media_streams)
                        .unwrap_or_default();
                    let audio_entries = build_track_entries(&streams, VideoTrackKind::Audio);
                    let subtitle_entries =
                        build_track_entries(&streams, VideoTrackKind::Subtitle);
                    BrowseResult::MediaOptions {
                        item_id,
                        versions,
                        audio_entries,
                        subtitle_entries,
                    }
                }
                Err(e) => BrowseResult::Error(format!("Couldn't load playback info: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Fetch `PlaybackInfo` and surface the audio or subtitle stream list.
    pub fn load_video_tracks(&mut self, item_id: String, kind: VideoTrackKind) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = match client.playback_info(&item_id).await {
                Ok(info) => {
                    let streams = info
                        .media_sources
                        .into_iter()
                        .next()
                        .map(|s| s.media_streams)
                        .unwrap_or_default();
                    BrowseResult::VideoTracks(build_track_entries(&streams, kind))
                }
                Err(e) => BrowseResult::Error(format!("Couldn't load tracks: {e}")),
            };
            let _ = tx.send(result);
        });
    }

    /// Fetch the Home dashboard payload in parallel: resume items, next-up,
    /// and the latest items per library (best-effort — each section degrades
    /// to empty on error so a single endpoint failing doesn't blank Home).
    pub fn load_home(&mut self, libraries: Vec<(String, String, Option<String>)>) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let resume_fut = client.resume_items();
            let next_up_fut = client.next_up(20);
            let latest_query = crate::api::models::ItemsQuery {
                recursive: Some(true),
                sort_by: vec!["DateCreated".to_string()],
                limit: Some(20),
                include_item_types: vec![
                    "Movie".to_string(),
                    "Series".to_string(),
                    "MusicAlbum".to_string(),
                ],
                fields: vec!["Overview".to_string()],
                ..Default::default()
            };
            let latest_fut = client.items(&latest_query);
            let (resume_res, next_up_res, latest_res) =
                tokio::join!(resume_fut, next_up_fut, latest_fut);

            let resume: Vec<Item> = match resume_res {
                Ok(r) => r.items.into_iter().map(super::item_from_dto).collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "home: resume_items failed");
                    Vec::new()
                }
            };
            let next_up: Vec<Item> = match next_up_res {
                Ok(r) => r.items.into_iter().map(super::item_from_dto).collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "home: next_up failed");
                    Vec::new()
                }
            };
            let latest_global: Vec<Item> = match latest_res {
                Ok(r) => r.items.into_iter().map(super::item_from_dto).collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "home: global latest failed");
                    Vec::new()
                }
            };

            let mut summaries: Vec<HomeLibrarySummary> = Vec::with_capacity(libraries.len());
            for (id, name, collection_type) in libraries {
                let recent = match client.latest_items(&id, 16).await {
                    Ok(items) => items.into_iter().map(super::item_from_dto).collect(),
                    Err(e) => {
                        tracing::warn!(library = %id, error = %e, "home: latest_items failed");
                        Vec::new()
                    }
                };
                // Best-effort total count; "1" limit + total_record_count gives
                // the library size without pulling every item.
                let item_count = client
                    .items(&crate::api::models::ItemsQuery {
                        parent_id: Some(id.clone()),
                        recursive: Some(true),
                        limit: Some(1),
                        ..Default::default()
                    })
                    .await
                    .map(|r| r.total_record_count)
                    .unwrap_or(0);
                summaries.push(HomeLibrarySummary {
                    id,
                    name,
                    collection_type,
                    item_count,
                    recent,
                });
            }

            let _ = tx.send(BrowseResult::Home(HomeData {
                resume,
                next_up,
                recent_per_library: summaries,
                latest_global,
                loading: false,
            }));
        });
    }

    /// Record a dislike (`POST /UserItems/{id}/Rating?likes=false`).
    pub fn dislike(&mut self, item_id: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            if let Err(e) = client.set_rating(&item_id, false).await {
                let _ = tx.send(BrowseResult::Error(format!("Dislike failed: {e}")));
            }
        });
    }

    /// Build the web URL for `item_id` and copy it to the system clipboard via
    /// OSC 52. The result feeds back through the regular channel so the status
    /// bar updates in the next tick.
    pub fn copy_item_url(&self, item_id: String, item_name: String) {
        let url = self.client.item_web_url(&item_id);
        let result = if super::error_modal::copy_to_clipboard(&url) {
            BrowseResult::Status(format!("Copied URL: {item_name}"))
        } else {
            BrowseResult::Error("Couldn't write OSC 52 clipboard sequence".to_string())
        };
        let _ = self.tx.send(result);
    }

    /// Deliver any completed fetches into the app's drill stack.
    pub fn tick(&mut self, app: &mut App) {
        while let Ok(result) = self.rx.try_recv() {
            match result {
                BrowseResult::Folder { id, items } => app.fill_level(&id, items),
                BrowseResult::Section {
                    library_id,
                    title,
                    items,
                } => app.apply_root_items(&library_id, title, items),
                BrowseResult::Search {
                    library_id,
                    query,
                    items,
                } => app.apply_root_items(&library_id, format!("Search: {query}"), items),
                BrowseResult::Failed { id, message } => {
                    app.drop_loading_level(&id);
                    app.show_error(message);
                }
                BrowseResult::Libraries(libraries) => app.replace_libraries(libraries),
                BrowseResult::LibraryMeta(entries) => app.set_library_picker_entries(entries),
                BrowseResult::Playlists(entries) => app.set_playlist_picker_entries(entries),
                BrowseResult::VideoTracks(entries) => app.set_video_track_picker_entries(entries),
                BrowseResult::MediaOptions {
                    item_id,
                    versions,
                    audio_entries,
                    subtitle_entries,
                } => app.set_media_options_view_data(
                    &item_id,
                    versions,
                    audio_entries,
                    subtitle_entries,
                ),
                BrowseResult::InstantMix(items) => {
                    let first = items[0].clone();
                    app.set_play_queue(items, 0);
                    app.queue_intent(Intent::PlayQueueCurrent { item: first });
                }
                BrowseResult::Status(message) => app.set_status(message),
                BrowseResult::Error(message) => app.show_error(message),
                BrowseResult::Home(data) => app.set_home_data(data),
                BrowseResult::GlobalSearch { query, items } => {
                    app.set_global_search_results(&query, items);
                }
            }
        }
    }

    /// Borrow the underlying [`JellyfinClient`] for callers that need to read
    /// the configured base URL (e.g. building the visible-library list before a
    /// sync).
    pub fn client(&self) -> &JellyfinClient {
        &self.client
    }
}

/// Compose a one-line label for a `MediaSource` (used by the version list).
fn format_version_label(source: &crate::api::models::MediaSource) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = source.name.as_deref().filter(|s| !s.is_empty()) {
        parts.push(name.to_string());
    }
    if let Some(container) = source.container.as_deref().filter(|s| !s.is_empty()) {
        parts.push(container.to_uppercase());
    }
    let video = source
        .media_streams
        .iter()
        .find(|s| s.type_.as_deref() == Some("Video"));
    if let Some(stream) = video {
        if let Some(codec) = stream.codec.as_deref().filter(|s| !s.is_empty()) {
            parts.push(codec.to_string());
        }
    }
    if let Some(size) = source.size {
        parts.push(format_size(size));
    }
    if parts.is_empty() {
        parts.push("Default".to_string());
    }
    parts.join("  ·  ")
}

fn format_size(bytes: i64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GiB", b / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.0} MiB", b / (1024.0 * 1024.0))
    } else {
        format!("{} B", bytes)
    }
}

/// Walk the source's stream list, build a picker entry list for `kind`.
/// `Auto` is always first; `Off` is included for subtitles. mpv's per-type
/// 1-based index is derived here so the caller can ignore Jellyfin's absolute
/// indices.
pub(crate) fn build_track_entries(
    streams: &[crate::api::models::MediaStream],
    kind: VideoTrackKind,
) -> Vec<TrackPickerEntry> {
    let wanted = match kind {
        VideoTrackKind::Audio => "Audio",
        VideoTrackKind::Subtitle => "Subtitle",
    };
    let mut entries: Vec<TrackPickerEntry> = Vec::new();
    entries.push(TrackPickerEntry {
        label: "Auto (server default)".to_string(),
        choice: TrackChoice::Auto,
    });
    if matches!(kind, VideoTrackKind::Subtitle) {
        entries.push(TrackPickerEntry {
            label: "Off".to_string(),
            choice: TrackChoice::Off,
        });
    }
    let mut counter = 0i32;
    for stream in streams {
        if stream.type_.as_deref() != Some(wanted) {
            continue;
        }
        counter += 1;
        entries.push(TrackPickerEntry {
            label: format_track_label(stream, counter),
            choice: TrackChoice::Pick(counter),
        });
    }
    entries
}

/// Compose a one-line description of `stream`, falling back to whatever
/// fields the server provides.
fn format_track_label(stream: &crate::api::models::MediaStream, mpv_index: i32) -> String {
    if let Some(display) = stream.display_title.as_deref().filter(|s| !s.is_empty()) {
        let mut s = format!("#{mpv_index}  {display}");
        if stream.is_default.unwrap_or(false) {
            s.push_str("  (default)");
        }
        if stream.is_forced.unwrap_or(false) {
            s.push_str("  (forced)");
        }
        return s;
    }
    let mut parts: Vec<String> = vec![format!("#{mpv_index}")];
    if let Some(lang) = stream.language.as_deref().filter(|s| !s.is_empty()) {
        parts.push(lang.to_string());
    }
    if let Some(title) = stream.title.as_deref().filter(|s| !s.is_empty()) {
        parts.push(title.to_string());
    }
    if let Some(codec) = stream.codec.as_deref().filter(|s| !s.is_empty()) {
        parts.push(codec.to_string());
    }
    if let Some(layout) = stream.channel_layout.as_deref().filter(|s| !s.is_empty()) {
        parts.push(layout.to_string());
    }
    if parts.len() == 1 {
        parts.push("Unnamed track".to_string());
    }
    parts.join("  ·  ")
}

/// Fetch all libraries + their top-level items, applying the visibility
/// ordering. Shared between the startup load and the runtime sync.
pub(crate) async fn fetch_libraries(
    client: &JellyfinClient,
    visible: Option<&[String]>,
) -> crate::api::Result<Vec<Library>> {
    let views = client.user_views().await?;
    let ordered = super::order_views(views, visible);
    let mut libraries = Vec::with_capacity(ordered.len());
    for view in ordered {
        let result = client
            .items(&ItemsQuery {
                parent_id: Some(view.id.clone()),
                limit: Some(200),
                fields: vec!["Overview".to_string()],
                ..Default::default()
            })
            .await?;
        libraries.push(Library {
            id: view.id,
            name: view.name.unwrap_or_else(|| "(library)".to_string()),
            collection_type: view.collection_type,
            items: result.items.into_iter().map(super::item_from_dto).collect(),
        });
    }
    Ok(libraries)
}


/// Build the items-query for a section filter and return the converted UI
/// [`Item`]s. Extracted so the network call can be exercised under wiremock
/// without spinning up the full [`Browser`].
pub(crate) async fn fetch_section_items(
    client: &crate::api::JellyfinClient,
    library_id: &str,
    section: &Section,
    extras: &SectionFilters,
) -> crate::api::Result<Vec<Item>> {
    // Runtime filters that force a wider search (recursive) when set even on
    // sections that would otherwise only show the immediate root level.
    let extras_force_recursive = !extras.genres.is_empty()
        || !extras.person_ids.is_empty()
        || !extras.studio_ids.is_empty()
        || !extras.years.is_empty()
        || !extras.tags.is_empty()
        || !extras.filters.is_empty();
    let query = ItemsQuery {
        parent_id: Some(library_id.to_string()),
        include_item_types: section.item_types.clone(),
        sort_by: extras
            .sort_override
            .clone()
            .unwrap_or_else(|| section.sort_by.clone()),
        recursive: Some(!section.item_types.is_empty() || extras_force_recursive),
        fields: vec!["Overview".to_string()],
        limit: Some(500),
        genres: extras.genres.clone(),
        person_ids: extras.person_ids.clone(),
        studio_ids: extras.studio_ids.clone(),
        years: extras.years.clone(),
        tags: extras.tags.clone(),
        filters: extras.filters.clone(),
        ..Default::default()
    };
    let result = client.items(&query).await?;
    Ok(result.items.into_iter().map(super::item_from_dto).collect())
}

/// Convert a [`crate::api::models::SearchHint`] into a UI [`Item`]. Hints
/// without any id are dropped (nothing playable / drillable to point at).
fn item_from_hint(hint: crate::api::models::SearchHint) -> Option<Item> {
    let id = hint.item_id.or(hint.id)?;
    Some(Item {
        id,
        name: hint.name.unwrap_or_else(|| "(untitled)".to_string()),
        kind: hint.type_,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::SearchHint;
    use crate::api::JellyfinClient;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn client_for(server: &MockServer) -> JellyfinClient {
        JellyfinClient::new(&server.uri(), "tok", "u1", "dev-1").unwrap()
    }

    #[tokio::test]
    async fn fetch_section_items_passes_filter_and_maps_items() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Users/u1/Items"))
            .and(query_param("parentId", "lib1"))
            .and(query_param("includeItemTypes", "MusicAlbum"))
            .and(query_param("recursive", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [
                    { "Id": "a1", "Name": "Discovery", "Type": "MusicAlbum" },
                    { "Id": "a2", "Name": "Random Access Memories", "Type": "MusicAlbum" }
                ],
                "TotalRecordCount": 2,
                "StartIndex": 0
            })))
            .mount(&server)
            .await;

        let section = Section {
            name: "Albums".to_string(),
            item_types: vec!["MusicAlbum".to_string()],
            sort_by: vec!["SortName".to_string()],
        };
        let items = fetch_section_items(
            &client_for(&server).await,
            "lib1",
            &section,
            &SectionFilters::default(),
        )
        .await
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "Discovery");
        assert_eq!(items[0].id, "a1");
    }

    #[tokio::test]
    async fn fetch_section_items_drops_recursive_for_empty_filter() {
        let server = MockServer::start().await;
        // The "All" section has no item-type filter; we expect recursive=false
        // so the server returns the library's direct children only.
        Mock::given(method("GET"))
            .and(path("/Users/u1/Items"))
            .and(query_param("parentId", "lib1"))
            .and(query_param("recursive", "false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [{ "Id": "x", "Name": "Root", "Type": "Folder" }],
                "TotalRecordCount": 1,
                "StartIndex": 0
            })))
            .mount(&server)
            .await;
        let section = Section {
            name: "All".to_string(),
            item_types: Vec::new(),
            sort_by: vec!["SortName".to_string()],
        };
        let items = fetch_section_items(
            &client_for(&server).await,
            "lib1",
            &section,
            &SectionFilters::default(),
        )
        .await
        .unwrap();
        assert_eq!(items[0].name, "Root");
    }

    #[test]
    fn build_track_entries_lists_audio_with_per_type_index() {
        use crate::api::models::MediaStream;
        let streams = vec![
            MediaStream {
                type_: Some("Video".to_string()),
                index: 0,
                ..Default::default()
            },
            MediaStream {
                type_: Some("Audio".to_string()),
                index: 1,
                language: Some("eng".to_string()),
                channel_layout: Some("5.1".to_string()),
                is_default: Some(true),
                ..Default::default()
            },
            MediaStream {
                type_: Some("Audio".to_string()),
                index: 2,
                language: Some("jpn".to_string()),
                ..Default::default()
            },
            MediaStream {
                type_: Some("Subtitle".to_string()),
                index: 3,
                language: Some("eng".to_string()),
                ..Default::default()
            },
        ];
        let audio = build_track_entries(&streams, VideoTrackKind::Audio);
        assert!(matches!(audio[0].choice, TrackChoice::Auto));
        assert!(matches!(audio[1].choice, TrackChoice::Pick(1)));
        assert!(matches!(audio[2].choice, TrackChoice::Pick(2)));
        assert_eq!(audio.len(), 3, "no Off for audio");

        let subs = build_track_entries(&streams, VideoTrackKind::Subtitle);
        assert!(matches!(subs[0].choice, TrackChoice::Auto));
        assert!(matches!(subs[1].choice, TrackChoice::Off));
        assert!(matches!(subs[2].choice, TrackChoice::Pick(1)));
        assert_eq!(subs.len(), 3);
    }

    #[test]
    fn item_from_hint_uses_item_id_then_id_then_drops() {
        let hint = SearchHint {
            item_id: Some("a".to_string()),
            id: Some("b".to_string()),
            name: Some("Track".to_string()),
            type_: Some("Audio".to_string()),
        };
        let item = item_from_hint(hint).expect("hint with item_id maps");
        assert_eq!(item.id, "a");
        assert_eq!(item.name, "Track");
        assert_eq!(item.kind.as_deref(), Some("Audio"));

        let no_item_id = SearchHint {
            item_id: None,
            id: Some("fallback".to_string()),
            ..Default::default()
        };
        assert_eq!(item_from_hint(no_item_id).unwrap().id, "fallback");

        let no_id = SearchHint::default();
        assert!(item_from_hint(no_id).is_none());
    }

    #[test]
    fn item_from_hint_supplies_placeholder_name() {
        let hint = SearchHint {
            item_id: Some("x".to_string()),
            name: None,
            ..Default::default()
        };
        assert_eq!(item_from_hint(hint).unwrap().name, "(untitled)");
    }
}
