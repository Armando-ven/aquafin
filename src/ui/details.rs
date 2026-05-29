//! On-demand item detail fetcher: when the user lingers on an item, fetch its
//! full [`BaseItemDto`] (for cast/credits), lyrics (audio), and children /
//! siblings (tvshows). Results land in
//! [`crate::ui::app::App::current_detail`] for the right-column context panes
//! to render.

use std::sync::mpsc::{self, Receiver, Sender};

use tokio::runtime::Handle;

use crate::api::models::ItemsQuery;
use crate::api::JellyfinClient;

use super::app::{App, ItemDetail, LyricLine, Person};

/// One completed detail fetch. The `id` lets the UI ignore stale responses
/// after the selection has moved on.
struct DetailResult {
    id: String,
    detail: ItemDetail,
}

pub struct Details {
    rt: Handle,
    client: JellyfinClient,
    /// Item id of the most recent fetch request, so we don't refire for the
    /// same selection.
    last_requested: Option<String>,
    tx: Sender<DetailResult>,
    rx: Receiver<DetailResult>,
}

impl Details {
    pub fn new(rt: Handle, client: JellyfinClient) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            rt,
            client,
            last_requested: None,
            tx,
            rx,
        }
    }

    /// Begin loading the detail for `item_id` if it differs from the most
    /// recent request. `kind` (`Audio`, `Movie`, `Series`, …) decides which
    /// supplementary endpoints to call (lyrics, children, siblings).
    pub fn request(&mut self, item_id: &str, kind: Option<&str>) {
        if item_id.is_empty() || self.last_requested.as_deref() == Some(item_id) {
            return;
        }
        self.last_requested = Some(item_id.to_string());
        let client = self.client.clone();
        let tx = self.tx.clone();
        let id = item_id.to_string();
        let is_audio = matches!(kind, Some("Audio" | "AudioBook"));
        let wants_children = matches!(kind, Some("Series" | "Season"));
        let wants_siblings = matches!(kind, Some("Episode" | "Season"));
        self.rt.spawn(async move {
            match fetch_detail(&client, &id, is_audio, wants_children, wants_siblings).await {
                Some(detail) => {
                    let _ = tx.send(DetailResult { id, detail });
                }
                None => {}
            }
        });
    }

    /// Deliver completed fetches into [`App::set_current_detail`].
    pub fn tick(&mut self, app: &mut App) {
        while let Ok(result) = self.rx.try_recv() {
            app.set_current_detail(&result.id, result.detail);
        }
    }
}

/// Pure async detail fetch. Returns `None` only when the primary item endpoint
/// fails (the user has no detail to render and we just skip the update).
pub(crate) async fn fetch_detail(
    client: &JellyfinClient,
    id: &str,
    is_audio: bool,
    wants_children: bool,
    wants_siblings: bool,
) -> Option<ItemDetail> {
    let item = match client.item(id).await {
        Ok(dto) => dto,
        Err(e) => {
            tracing::warn!(item = %id, error = %e, "couldn't fetch item detail");
            return None;
        }
    };
    let cast = item
        .people
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|p| Person {
            name: p.name.unwrap_or_default(),
            role: p.role,
            kind: p.type_,
        })
        .collect();
    let genres = item.genres.clone().unwrap_or_default();
    // Lyrics are a music-only concept; the endpoint 404s for everything else,
    // so we just don't ask.
    let lyrics = if is_audio {
        client.lyrics(id).await.ok().map(|l| {
            l.lyrics
                .into_iter()
                .map(|line| LyricLine {
                    text: line.text,
                    start_ticks: line.start,
                })
                .collect()
        })
    } else {
        None
    };
    let children = if wants_children {
        fetch_children(client, id).await
    } else {
        Vec::new()
    };
    let siblings = if wants_siblings {
        match &item.parent_id {
            Some(parent_id) => {
                let mut sibs = fetch_children(client, parent_id).await;
                sibs.retain(|sib| sib.id != id);
                sibs
            }
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    Some(ItemDetail {
        cast,
        genres,
        lyrics,
        children,
        siblings,
    })
}

async fn fetch_children(client: &JellyfinClient, parent_id: &str) -> Vec<super::app::Item> {
    let query = ItemsQuery {
        parent_id: Some(parent_id.to_string()),
        sort_by: vec![
            "ParentIndexNumber".to_string(),
            "IndexNumber".to_string(),
            "SortName".to_string(),
        ],
        limit: Some(200),
        ..Default::default()
    };
    match client.items(&query).await {
        Ok(result) => result
            .items
            .into_iter()
            .map(super::item_from_dto)
            .collect(),
        Err(e) => {
            tracing::warn!(parent = %parent_id, error = %e, "couldn't fetch children");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn client_for(server: &MockServer) -> JellyfinClient {
        JellyfinClient::new(&server.uri(), "tok", "u1", "dev-1").unwrap()
    }

    #[tokio::test]
    async fn fetch_detail_extracts_cast_and_genres() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/m1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Id": "m1",
                "Name": "The Matrix",
                "Type": "Movie",
                "Genres": ["Sci-Fi"],
                "People": [
                    { "Id": "p1", "Name": "Neo Reeves", "Type": "Actor", "Role": "Neo" }
                ]
            })))
            .mount(&server)
            .await;

        let detail = fetch_detail(&client_for(&server).await, "m1", false, false, false)
            .await
            .unwrap();
        assert_eq!(detail.cast.len(), 1);
        assert_eq!(detail.cast[0].name, "Neo Reeves");
        assert_eq!(detail.cast[0].role.as_deref(), Some("Neo"));
        assert_eq!(detail.genres, vec!["Sci-Fi"]);
        assert!(detail.lyrics.is_none());
        assert!(detail.children.is_empty());
        assert!(detail.siblings.is_empty());
    }

    #[tokio::test]
    async fn fetch_detail_loads_children_when_requested() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/series1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Id": "series1",
                "Name": "Severance",
                "Type": "Series"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Users/u1/Items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [
                    { "Id": "s1", "Name": "Season 1", "Type": "Season" },
                    { "Id": "s2", "Name": "Season 2", "Type": "Season" }
                ],
                "TotalRecordCount": 2,
                "StartIndex": 0
            })))
            .mount(&server)
            .await;

        let detail = fetch_detail(&client_for(&server).await, "series1", false, true, false)
            .await
            .unwrap();
        assert_eq!(detail.children.len(), 2);
        assert_eq!(detail.children[0].name, "Season 1");
    }

    #[tokio::test]
    async fn fetch_detail_collects_siblings_minus_self() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/ep2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Id": "ep2",
                "Name": "Episode 2",
                "Type": "Episode",
                "ParentId": "season1"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Users/u1/Items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [
                    { "Id": "ep1", "Name": "Episode 1", "Type": "Episode" },
                    { "Id": "ep2", "Name": "Episode 2", "Type": "Episode" },
                    { "Id": "ep3", "Name": "Episode 3", "Type": "Episode" }
                ],
                "TotalRecordCount": 3,
                "StartIndex": 0
            })))
            .mount(&server)
            .await;

        let detail = fetch_detail(&client_for(&server).await, "ep2", false, false, true)
            .await
            .unwrap();
        assert_eq!(detail.siblings.len(), 2);
        assert!(detail.siblings.iter().all(|s| s.id != "ep2"));
    }
}
