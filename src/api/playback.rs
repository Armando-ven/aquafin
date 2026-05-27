//! Playback reporting to the server (used by the video and audio players).

use super::client::JellyfinClient;
use super::models::{PlaybackProgressInfo, PlaybackStartInfo, PlaybackStopInfo};
use super::Result;

impl JellyfinClient {
    /// `POST /Sessions/Playing` — report that playback has started.
    pub async fn report_playback_start(&self, info: &PlaybackStartInfo) -> Result<()> {
        self.send_no_content(self.post("/Sessions/Playing").json(info))
            .await
    }

    /// `POST /Sessions/Playing/Progress` — periodic progress heartbeat.
    pub async fn report_playback_progress(&self, info: &PlaybackProgressInfo) -> Result<()> {
        self.send_no_content(self.post("/Sessions/Playing/Progress").json(info))
            .await
    }

    /// `POST /Sessions/Playing/Stopped` — report that playback has stopped.
    pub async fn report_playback_stopped(&self, info: &PlaybackStopInfo) -> Result<()> {
        self.send_no_content(self.post("/Sessions/Playing/Stopped").json(info))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn client_for(server: &MockServer) -> JellyfinClient {
        JellyfinClient::new(&server.uri(), "tok", "u1", "dev-1").unwrap()
    }

    #[tokio::test]
    async fn start_sends_expected_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Sessions/Playing"))
            .and(body_json(serde_json::json!({
                "ItemId": "itm1",
                "IsPaused": false,
                "CanSeek": true
            })))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let info = PlaybackStartInfo {
            item_id: "itm1".to_string(),
            can_seek: true,
            ..Default::default()
        };
        client_for(&server)
            .await
            .report_playback_start(&info)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn progress_and_stopped_succeed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Sessions/Playing/Progress"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Sessions/Playing/Stopped"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        client
            .report_playback_progress(&PlaybackProgressInfo {
                item_id: "itm1".to_string(),
                position_ticks: Some(30_000_000),
                ..Default::default()
            })
            .await
            .unwrap();
        client
            .report_playback_stopped(&PlaybackStopInfo {
                item_id: "itm1".to_string(),
                position_ticks: Some(60_000_000),
                ..Default::default()
            })
            .await
            .unwrap();
    }
}
