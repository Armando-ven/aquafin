//! The [`JellyfinClient`] and the shared request plumbing used across the API
//! submodules.

use std::time::Duration;

use serde::de::DeserializeOwned;

use super::models::{Credentials, ImageResponse, PublicSystemInfo};
use super::{Error, Result};

const CLIENT_NAME: &str = "aquafin";
const DEVICE_NAME: &str = "aquafin";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// An authenticated, read-only client for a single Jellyfin server + user.
#[derive(Debug, Clone)]
pub struct JellyfinClient {
    base_url: String,
    user_id: String,
    device_id: String,
    token: String,
    auth_header: String,
    http: reqwest::Client,
}

impl JellyfinClient {
    /// Build a client from an existing token. `base_url` is normalised (trailing
    /// slashes stripped); the auth header is precomputed once.
    pub fn new(base_url: &str, token: &str, user_id: &str, device_id: &str) -> Result<Self> {
        Ok(Self {
            base_url: normalize_base(base_url),
            user_id: user_id.to_string(),
            device_id: device_id.to_string(),
            token: token.to_string(),
            auth_header: auth_header_value(device_id, Some(token)),
            http: build_http_client()?,
        })
    }

    /// Build a client from persisted [`Credentials`].
    pub fn from_credentials(creds: &Credentials) -> Result<Self> {
        Self::new(
            &creds.server_url,
            &creds.token,
            &creds.user_id,
            &creds.device_id,
        )
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Build a direct-play video URL for `mpv`. The token rides in the query
    /// string (`api_key`) because mpv opens the URL itself and can't set our
    /// `X-Emby-Authorization` header. `static=true` asks the server to stream the
    /// original file without remuxing.
    pub fn video_stream_url(&self, item_id: &str) -> String {
        format!(
            "{}/Videos/{}/stream?static=true&mediaSourceId={}&api_key={}",
            self.base_url,
            urlencode(item_id),
            urlencode(item_id),
            urlencode(&self.token),
        )
    }

    /// Build a URL for the universal audio endpoint.
    ///
    /// `containers` and `audio_codecs` are the formats/codecs we can decode. The
    /// server direct-streams when the source matches (preserving quality, e.g.
    /// for FLAC) and otherwise transcodes to the *first* listed codec. We must
    /// list `audio_codecs` so the server knows our codec support: passing only
    /// `container` lets it direct-stream an Opus stream we can't decode. Opus is
    /// excluded from `audio_codecs`, so Opus sources transcode to the first codec.
    pub fn audio_universal_url(
        &self,
        item_id: &str,
        containers: &str,
        audio_codecs: &str,
    ) -> String {
        format!(
            "{}/Audio/{}/universal?userId={}&deviceId={}&container={}&audioCodec={}&api_key={}",
            self.base_url,
            urlencode(item_id),
            urlencode(&self.user_id),
            urlencode(&self.device_id),
            urlencode(containers),
            urlencode(audio_codecs),
            urlencode(&self.token),
        )
    }

    /// Download a track's bytes from the universal audio endpoint for in-app
    /// playback. Held in memory and handed to the audio decoder. Uses a generous
    /// per-request timeout: lossless tracks can be tens of MB and the client's
    /// default API timeout would cut a slow download off mid-stream.
    pub async fn audio_bytes(
        &self,
        item_id: &str,
        containers: &str,
        audio_codecs: &str,
    ) -> Result<Vec<u8>> {
        let url = self.audio_universal_url(item_id, containers, audio_codecs);
        let request = self.http.get(url).timeout(Duration::from_secs(600));
        let resp = ensure_success(request.send().await?).await?;
        let bytes = resp.bytes().await?.to_vec();
        tracing::debug!(item_id, bytes = bytes.len(), "audio track downloaded");
        Ok(bytes)
    }

    pub(crate) fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        self.http
            .request(method, url)
            .header("X-Emby-Authorization", self.auth_header.as_str())
    }

    pub(crate) fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    pub(crate) fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::POST, path)
    }

    pub(crate) async fn send_json<T: DeserializeOwned>(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<T> {
        let resp = ensure_success(builder.send().await?).await?;
        Ok(resp.json::<T>().await?)
    }

    pub(crate) async fn send_no_content(&self, builder: reqwest::RequestBuilder) -> Result<()> {
        ensure_success(builder.send().await?).await?;
        Ok(())
    }

    pub(crate) async fn send_bytes(&self, builder: reqwest::RequestBuilder) -> Result<ImageResponse> {
        let resp = ensure_success(builder.send().await?).await?;
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = resp.bytes().await?.to_vec();
        Ok(ImageResponse {
            content_type,
            bytes,
        })
    }
}

/// A `reqwest::Client` configured with aquafin's timeout and user agent.
pub(crate) fn build_http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("aquafin/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Fetch unauthenticated server info from `GET /System/Info/Public`. Used to
/// validate that a URL points at a reachable Jellyfin server.
pub async fn fetch_public_info(server_url: &str) -> Result<PublicSystemInfo> {
    let base = normalize_base(server_url);
    let resp = build_http_client()?
        .get(format!("{base}/System/Info/Public"))
        .send()
        .await?;
    Ok(ensure_success(resp).await?.json().await?)
}

/// Build the `X-Emby-Authorization` header value, optionally carrying a token.
pub(crate) fn auth_header_value(device_id: &str, token: Option<&str>) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let token_part = token
        .map(|t| format!(", Token=\"{t}\""))
        .unwrap_or_default();
    format!(
        "MediaBrowser Client=\"{CLIENT_NAME}\", Device=\"{DEVICE_NAME}\", \
         DeviceId=\"{device_id}\", Version=\"{version}\"{token_part}"
    )
}

/// Strip trailing slashes so paths can be appended predictably.
pub(crate) fn normalize_base(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// Percent-encode a query-string value. Only the RFC 3986 unreserved set passes
/// through untouched; everything else is `%`-escaped. Enough for the ids, tokens
/// and short container lists aquafin puts in URLs.
pub(crate) fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Return the response if it has a 2xx status, otherwise an [`Error::Status`]
/// carrying the body text.
pub(crate) async fn ensure_success(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else {
        let code = status.as_u16();
        let message = resp.text().await.unwrap_or_else(|_| status.to_string());
        Err(Error::Status {
            status: code,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn video_stream_url_is_direct_play_with_api_key() {
        let client = JellyfinClient::new("https://jelly.example/", "tok123", "u1", "dev-1").unwrap();
        let url = client.video_stream_url("itm1");
        assert_eq!(
            url,
            "https://jelly.example/Videos/itm1/stream?static=true&mediaSourceId=itm1&api_key=tok123"
        );
    }

    #[test]
    fn audio_universal_url_carries_codecs_user_device_and_token() {
        let client = JellyfinClient::new("https://jelly.example", "tok123", "u1", "dev-1").unwrap();
        let url = client.audio_universal_url("itm1", "mp3,flac", "aac,mp3,flac");
        assert!(url.starts_with("https://jelly.example/Audio/itm1/universal?"));
        assert!(url.contains("userId=u1"));
        assert!(url.contains("deviceId=dev-1"));
        assert!(url.contains("container=mp3%2Cflac"));
        assert!(url.contains("audioCodec=aac%2Cmp3%2Cflac"));
        assert!(url.contains("api_key=tok123"));
    }

    #[tokio::test]
    async fn audio_bytes_downloads_track() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Audio/itm1/universal"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![9u8, 8, 7], "audio/mpeg"))
            .mount(&server)
            .await;
        let client = JellyfinClient::new(&server.uri(), "tok", "u1", "dev-1").unwrap();
        let bytes = client.audio_bytes("itm1", "mp3", "mp3").await.unwrap();
        assert_eq!(bytes, vec![9, 8, 7]);
    }

    #[test]
    fn urlencode_escapes_reserved() {
        assert_eq!(urlencode("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(urlencode("Abc-1.0_x~y"), "Abc-1.0_x~y");
    }

    #[tokio::test]
    async fn fetch_public_info_parses_server_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/System/Info/Public"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ServerName": "My Jellyfin",
                "Version": "10.10.0",
                "Id": "abc",
                "ProductName": "Jellyfin Server"
            })))
            .mount(&server)
            .await;

        let info = fetch_public_info(&server.uri()).await.unwrap();
        assert_eq!(info.server_name.as_deref(), Some("My Jellyfin"));
        assert_eq!(info.version.as_deref(), Some("10.10.0"));
    }

    #[tokio::test]
    async fn fetch_public_info_errors_on_non_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/System/Info/Public"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(fetch_public_info(&server.uri()).await.is_err());
    }
}
