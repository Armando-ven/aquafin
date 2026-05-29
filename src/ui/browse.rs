//! On-demand library queries: folder drilling, section filters, and search.
//!
//! All fetches run on the async runtime; results come back to the UI thread
//! over a channel and fill the matching loading level. The UI thread never
//! blocks.

use std::sync::mpsc::{self, Receiver, Sender};

use tokio::runtime::Handle;

use crate::api::models::ItemsQuery;
use crate::api::JellyfinClient;

use super::app::{App, Item, Section};

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
    Failed {
        id: String,
        message: String,
    },
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
    pub fn apply_section(&mut self, library_id: String, library_name: String, section: Section) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        let title = format!("{library_name} · {}", section.name);
        self.rt.spawn(async move {
            let result = match fetch_section_items(&client, &library_id, &section).await {
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

    /// Run a search via `/Search/Hints`. Results replace the active library's
    /// root level.
    pub fn search(&mut self, library_id: String, query: String) {
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
            }
        }
    }
}

/// Build the items-query for a section filter and return the converted UI
/// [`Item`]s. Extracted so the network call can be exercised under wiremock
/// without spinning up the full [`Browser`].
pub(crate) async fn fetch_section_items(
    client: &crate::api::JellyfinClient,
    library_id: &str,
    section: &Section,
) -> crate::api::Result<Vec<Item>> {
    let query = ItemsQuery {
        parent_id: Some(library_id.to_string()),
        include_item_types: section.item_types.clone(),
        sort_by: section.sort_by.clone(),
        // Recursive only when there's a type filter; without one we're showing
        // the library's direct top-level items.
        recursive: Some(!section.item_types.is_empty()),
        fields: vec!["Overview".to_string()],
        limit: Some(500),
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
        let items = fetch_section_items(&client_for(&server).await, "lib1", &section)
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
        let items = fetch_section_items(&client_for(&server).await, "lib1", &section)
            .await
            .unwrap();
        assert_eq!(items[0].name, "Root");
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
