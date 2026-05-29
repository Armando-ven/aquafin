//! Read-only library queries: views, item listings, detail, images, resume, search.

use super::client::JellyfinClient;
use super::models::{BaseItemDto, ImageResponse, ItemsQuery, ItemsResult, LyricsDto, SearchHintResult};
use super::Result;

impl JellyfinClient {
    /// `GET /UserViews` — the user's libraries.
    pub async fn user_views(&self) -> Result<Vec<BaseItemDto>> {
        let result: ItemsResult = self
            .send_json(self.get("/UserViews").query(&[("userId", self.user_id())]))
            .await?;
        Ok(result.items)
    }

    /// `GET /Users/{userId}/Items` — list items, filtered/paged via `query`.
    pub async fn items(&self, query: &ItemsQuery) -> Result<ItemsResult> {
        let path = format!("/Users/{}/Items", self.user_id());
        self.send_json(self.get(&path).query(&query.to_pairs())).await
    }

    /// `GET /Items/{itemId}` — full detail for a single item (user-scoped).
    pub async fn item(&self, item_id: &str) -> Result<BaseItemDto> {
        let path = format!("/Items/{item_id}");
        self.send_json(self.get(&path).query(&[("userId", self.user_id())]))
            .await
    }

    /// `GET /UserItems/Resume` — Continue Watching.
    pub async fn resume_items(&self) -> Result<ItemsResult> {
        self.send_json(
            self.get("/UserItems/Resume")
                .query(&[("userId", self.user_id())]),
        )
        .await
    }

    /// `GET /Search/Hints` — quick search suggestions.
    pub async fn search_hints(&self, search_term: &str) -> Result<SearchHintResult> {
        self.send_json(self.get("/Search/Hints").query(&[
            ("searchTerm", search_term),
            ("userId", self.user_id()),
        ]))
        .await
    }

    /// `POST` or `DELETE /Users/{userId}/FavoriteItems/{itemId}` — toggle favorite.
    pub async fn set_favorite(&self, item_id: &str, favorite: bool) -> Result<()> {
        let path = format!("/Users/{}/FavoriteItems/{}", self.user_id(), item_id);
        let method = if favorite {
            reqwest::Method::POST
        } else {
            reqwest::Method::DELETE
        };
        self.send_no_content(self.request(method, &path)).await
    }

    /// `GET /Items/{itemId}/Lyrics` — synced or plain-text lyrics for an audio
    /// item. Errors when the server has no lyrics for the track.
    pub async fn lyrics(&self, item_id: &str) -> Result<LyricsDto> {
        let path = format!("/Items/{item_id}/Lyrics");
        self.send_json(self.get(&path)).await
    }

    /// `GET /Items/{itemId}/Images/Primary` — primary image bytes.
    pub async fn primary_image(
        &self,
        item_id: &str,
        max_width: Option<u32>,
    ) -> Result<ImageResponse> {
        let path = format!("/Items/{item_id}/Images/Primary");
        let mut builder = self.get(&path);
        if let Some(width) = max_width {
            builder = builder.query(&[("maxWidth", width.to_string())]);
        }
        self.send_bytes(builder).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn client_for(server: &MockServer) -> JellyfinClient {
        JellyfinClient::new(&server.uri(), "tok", "u1", "dev-1").unwrap()
    }

    #[tokio::test]
    async fn user_views_parses_libraries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/UserViews"))
            .and(query_param("userId", "u1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [
                    { "Id": "v1", "Name": "Movies", "CollectionType": "movies" },
                    { "Id": "v2", "Name": "Shows", "CollectionType": "tvshows" }
                ],
                "TotalRecordCount": 2
            })))
            .mount(&server)
            .await;

        let views = client_for(&server).await.user_views().await.unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].name.as_deref(), Some("Movies"));
        assert_eq!(views[0].collection_type.as_deref(), Some("movies"));
    }

    #[tokio::test]
    async fn items_sends_query_params() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Users/u1/Items"))
            .and(query_param("parentId", "lib1"))
            .and(query_param("includeItemTypes", "Movie"))
            .and(query_param("limit", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Items": [ { "Id": "i1", "Name": "A" }, { "Id": "i2", "Name": "B" } ],
                "TotalRecordCount": 2,
                "StartIndex": 0
            })))
            .mount(&server)
            .await;

        let query = ItemsQuery {
            parent_id: Some("lib1".to_string()),
            include_item_types: vec!["Movie".to_string()],
            limit: Some(2),
            ..Default::default()
        };
        let result = client_for(&server).await.items(&query).await.unwrap();
        assert_eq!(result.total_record_count, 2);
        assert_eq!(result.items.len(), 2);
    }

    #[tokio::test]
    async fn item_detail_parses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/itm1"))
            .and(query_param("userId", "u1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Id": "itm1",
                "Name": "The Matrix",
                "Type": "Movie",
                "ProductionYear": 1999,
                "UserData": { "PlaybackPositionTicks": 12000000, "Played": false }
            })))
            .mount(&server)
            .await;

        let item = client_for(&server).await.item("itm1").await.unwrap();
        assert_eq!(item.name.as_deref(), Some("The Matrix"));
        assert_eq!(item.type_.as_deref(), Some("Movie"));
        assert_eq!(item.production_year, Some(1999));
        assert_eq!(
            item.user_data.and_then(|u| u.playback_position_ticks),
            Some(12_000_000)
        );
    }

    #[tokio::test]
    async fn non_success_status_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/bad"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client_for(&server).await.item("bad").await.unwrap_err();
        assert!(
            matches!(err, crate::api::Error::Status { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn search_hints_parses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Search/Hints"))
            .and(query_param("searchTerm", "matrix"))
            .and(query_param("userId", "u1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "SearchHints": [ { "ItemId": "itm1", "Name": "The Matrix", "Type": "Movie" } ],
                "TotalRecordCount": 1
            })))
            .mount(&server)
            .await;

        let hints = client_for(&server)
            .await
            .search_hints("matrix")
            .await
            .unwrap();
        assert_eq!(hints.total_record_count, 1);
        assert_eq!(hints.search_hints[0].name.as_deref(), Some("The Matrix"));
    }

    #[tokio::test]
    async fn item_detail_carries_people_and_genres() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/itm1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Id": "itm1",
                "Name": "The Matrix",
                "Type": "Movie",
                "Genres": ["Sci-Fi", "Action"],
                "People": [
                    { "Id": "p1", "Name": "Keanu Reeves", "Type": "Actor", "Role": "Neo" },
                    { "Id": "p2", "Name": "Lana Wachowski", "Type": "Director" }
                ]
            })))
            .mount(&server)
            .await;

        let item = client_for(&server).await.item("itm1").await.unwrap();
        let genres = item.genres.unwrap();
        assert_eq!(genres, vec!["Sci-Fi", "Action"]);
        let people = item.people.unwrap();
        assert_eq!(people.len(), 2);
        assert_eq!(people[0].name.as_deref(), Some("Keanu Reeves"));
        assert_eq!(people[0].role.as_deref(), Some("Neo"));
        assert_eq!(people[0].type_.as_deref(), Some("Actor"));
        assert_eq!(people[1].type_.as_deref(), Some("Director"));
    }

    #[tokio::test]
    async fn lyrics_endpoint_parses_synced_and_plain() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/trk1/Lyrics"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Lyrics": [
                    { "Text": "Line one", "Start": 0 },
                    { "Text": "Line two", "Start": 30000000 },
                    { "Text": "Untimed line" }
                ]
            })))
            .mount(&server)
            .await;
        let lyrics = client_for(&server).await.lyrics("trk1").await.unwrap();
        assert_eq!(lyrics.lyrics.len(), 3);
        assert_eq!(lyrics.lyrics[0].text, "Line one");
        assert_eq!(lyrics.lyrics[0].start, Some(0));
        assert_eq!(lyrics.lyrics[1].start, Some(30_000_000));
        assert_eq!(lyrics.lyrics[2].start, None);
    }

    #[tokio::test]
    async fn primary_image_returns_bytes_and_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Items/itm1/Images/Primary"))
            .and(query_param("maxWidth", "300"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![1u8, 2, 3, 4], "image/jpeg"))
            .mount(&server)
            .await;

        let image = client_for(&server)
            .await
            .primary_image("itm1", Some(300))
            .await
            .unwrap();
        assert_eq!(image.bytes, vec![1, 2, 3, 4]);
        assert_eq!(image.content_type.as_deref(), Some("image/jpeg"));
    }
}
