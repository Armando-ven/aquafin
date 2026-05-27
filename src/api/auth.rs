//! Authentication flows (username/password + Quick Connect) and persistence of
//! the resulting [`Credentials`].

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use super::client::{auth_header_value, build_http_client, ensure_success, normalize_base};
use super::models::{
    AuthenticateByNameRequest, AuthenticationResult, Credentials, QuickConnectAuthRequest,
    QuickConnectResult,
};
use super::{Error, Result};

/// Generate a fresh, stable-per-install device identifier.
pub fn new_device_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Authenticate with a username and password via `POST /Users/AuthenticateByName`.
pub async fn authenticate_by_name(
    server_url: &str,
    device_id: &str,
    username: &str,
    password: &str,
) -> Result<Credentials> {
    let base = normalize_base(server_url);
    let resp = build_http_client()?
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("X-Emby-Authorization", auth_header_value(device_id, None))
        .json(&AuthenticateByNameRequest {
            username,
            pw: password,
        })
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Error::AuthFailed);
    }
    let auth: AuthenticationResult = ensure_success(resp).await?.json().await?;
    Ok(credentials_from(base, device_id, auth))
}

/// Begin a Quick Connect request via `POST /QuickConnect/Initiate`. The returned
/// result carries the `Code` to show the user and the `Secret` to poll with.
pub async fn quick_connect_initiate(
    server_url: &str,
    device_id: &str,
) -> Result<QuickConnectResult> {
    let base = normalize_base(server_url);
    let resp = build_http_client()?
        .post(format!("{base}/QuickConnect/Initiate"))
        .header("X-Emby-Authorization", auth_header_value(device_id, None))
        .send()
        .await?;
    Ok(ensure_success(resp).await?.json().await?)
}

/// Poll `GET /QuickConnect/Connect` once; check [`QuickConnectResult::authenticated`].
pub async fn quick_connect_poll(
    server_url: &str,
    device_id: &str,
    secret: &str,
) -> Result<QuickConnectResult> {
    let base = normalize_base(server_url);
    let resp = build_http_client()?
        .get(format!("{base}/QuickConnect/Connect"))
        .query(&[("Secret", secret)])
        .header("X-Emby-Authorization", auth_header_value(device_id, None))
        .send()
        .await?;
    Ok(ensure_success(resp).await?.json().await?)
}

/// Once Quick Connect is approved, exchange the secret for a token via
/// `POST /Users/AuthenticateWithQuickConnect`.
pub async fn quick_connect_authenticate(
    server_url: &str,
    device_id: &str,
    secret: &str,
) -> Result<Credentials> {
    let base = normalize_base(server_url);
    let resp = build_http_client()?
        .post(format!("{base}/Users/AuthenticateWithQuickConnect"))
        .header("X-Emby-Authorization", auth_header_value(device_id, None))
        .json(&QuickConnectAuthRequest { secret })
        .send()
        .await?;
    let auth: AuthenticationResult = ensure_success(resp).await?.json().await?;
    Ok(credentials_from(base, device_id, auth))
}

fn credentials_from(server_url: String, device_id: &str, auth: AuthenticationResult) -> Credentials {
    Credentials {
        server_url,
        user_id: auth.user.id,
        token: auth.access_token,
        device_id: device_id.to_string(),
    }
}

// --- Persistence ------------------------------------------------------------

const KEYRING_SERVICE: &str = "aquafin";

/// Abstraction over secret storage so the OS keyring can be faked in tests.
trait TokenStore {
    fn set_token(&self, account: &str, token: &str) -> Result<()>;
    fn get_token(&self, account: &str) -> Result<Option<String>>;
}

/// Stores the token in the OS Secret Service (GNOME Keyring / KWallet, etc.).
struct KeyringStore;

impl TokenStore for KeyringStore {
    fn set_token(&self, account: &str, token: &str) -> Result<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, account).map_err(map_keyring)?;
        entry.set_password(token).map_err(map_keyring)
    }

    fn get_token(&self, account: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, account).map_err(map_keyring)?;
        match entry.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_keyring(e)),
        }
    }
}

fn map_keyring(e: keyring::Error) -> Error {
    Error::Keyring(e.to_string())
}

/// On-disk form. Non-secret fields are always written; `token` is only present
/// as a fallback for when the OS keyring is unavailable.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredCredentials {
    server_url: String,
    user_id: String,
    device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

/// Save credentials: the token goes to the OS keyring when available, otherwise
/// into the `0600` file. Non-secret fields always go to the file.
pub fn save_credentials(creds: &Credentials) -> Result<()> {
    save_credentials_with(&KeyringStore, &default_path()?, creds)
}

/// Load credentials from the default location, or `None` if not logged in.
pub fn load_credentials() -> Result<Option<Credentials>> {
    load_credentials_with(&KeyringStore, &default_path()?)
}

fn save_credentials_with(store: &dyn TokenStore, path: &Path, creds: &Credentials) -> Result<()> {
    let token_in_file = match store.set_token(&creds.user_id, &creds.token) {
        Ok(()) => None,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "OS keyring unavailable; storing token in the 0600 file instead"
            );
            Some(creds.token.clone())
        }
    };
    let stored = StoredCredentials {
        server_url: creds.server_url.clone(),
        user_id: creds.user_id.clone(),
        device_id: creds.device_id.clone(),
        token: token_in_file,
    };
    write_stored(path, &stored)
}

fn load_credentials_with(store: &dyn TokenStore, path: &Path) -> Result<Option<Credentials>> {
    let Some(stored) = read_stored(path)? else {
        return Ok(None);
    };
    let token = match stored.token {
        Some(token) => token,
        None => match store.get_token(&stored.user_id)? {
            Some(token) => token,
            None => return Ok(None),
        },
    };
    Ok(Some(Credentials {
        server_url: stored.server_url,
        user_id: stored.user_id,
        token,
        device_id: stored.device_id,
    }))
}

/// Write the stored form as TOML, with the file mode forced to `0600`.
fn write_stored(path: &Path, stored: &StoredCredentials) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized = toml::to_string_pretty(stored).map_err(|e| Error::Toml(e.to_string()))?;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(serialized.as_bytes())?;

    // `.mode()` only applies on creation, so enforce 0600 explicitly for overwrites.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn read_stored(path: &Path) -> Result<Option<StoredCredentials>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let stored: StoredCredentials =
                toml::from_str(&contents).map_err(|e| Error::Toml(e.to_string()))?;
            Ok(Some(stored))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

fn default_path() -> Result<std::path::PathBuf> {
    crate::paths::credentials_file().map_err(|e| Error::Path(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn auth_result_body(user_id: &str, token: &str) -> serde_json::Value {
        serde_json::json!({
            "User": { "Id": user_id, "Name": "alice" },
            "AccessToken": token,
            "ServerId": "srv1"
        })
    }

    #[tokio::test]
    async fn password_auth_parses_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateByName"))
            .and(body_json(serde_json::json!({ "Username": "alice", "Pw": "secret" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_result_body("u1", "tok1")))
            .mount(&server)
            .await;

        let creds = authenticate_by_name(&server.uri(), "dev-1", "alice", "secret")
            .await
            .unwrap();

        assert_eq!(creds.token, "tok1");
        assert_eq!(creds.user_id, "u1");
        assert_eq!(creds.device_id, "dev-1");
        assert_eq!(creds.server_url, server.uri().trim_end_matches('/'));
    }

    #[tokio::test]
    async fn password_auth_401_maps_to_auth_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateByName"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = authenticate_by_name(&server.uri(), "dev-1", "alice", "wrong")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::AuthFailed), "got {err:?}");
    }

    #[tokio::test]
    async fn quick_connect_full_flow() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/QuickConnect/Initiate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Authenticated": false,
                "Secret": "sec-1",
                "Code": "123456"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/QuickConnect/Connect"))
            .and(query_param("Secret", "sec-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Authenticated": true,
                "Secret": "sec-1",
                "Code": "123456"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateWithQuickConnect"))
            .and(body_json(serde_json::json!({ "Secret": "sec-1" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(auth_result_body("u2", "tok2")))
            .mount(&server)
            .await;

        let init = quick_connect_initiate(&server.uri(), "dev-1").await.unwrap();
        assert_eq!(init.code, "123456");
        assert_eq!(init.secret, "sec-1");

        let polled = quick_connect_poll(&server.uri(), "dev-1", &init.secret)
            .await
            .unwrap();
        assert!(polled.authenticated);

        let creds = quick_connect_authenticate(&server.uri(), "dev-1", &init.secret)
            .await
            .unwrap();
        assert_eq!(creds.token, "tok2");
        assert_eq!(creds.user_id, "u2");
    }

    struct MemStore {
        available: bool,
        map: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    impl MemStore {
        fn new(available: bool) -> Self {
            Self {
                available,
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }
        }
    }

    impl TokenStore for MemStore {
        fn set_token(&self, account: &str, token: &str) -> Result<()> {
            if !self.available {
                return Err(Error::Keyring("unavailable".into()));
            }
            self.map
                .lock()
                .unwrap()
                .insert(account.to_string(), token.to_string());
            Ok(())
        }

        fn get_token(&self, account: &str) -> Result<Option<String>> {
            if !self.available {
                return Err(Error::Keyring("unavailable".into()));
            }
            Ok(self.map.lock().unwrap().get(account).cloned())
        }
    }

    fn sample_creds() -> Credentials {
        Credentials {
            server_url: "https://jelly.example".to_string(),
            user_id: "u1".to_string(),
            token: "super-secret-token".to_string(),
            device_id: "dev-1".to_string(),
        }
    }

    #[test]
    fn keyring_path_keeps_token_out_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let store = MemStore::new(true);
        let creds = sample_creds();

        save_credentials_with(&store, &path, &creds).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains("super-secret-token"),
            "token must not be written to the file when the keyring is used:\n{on_disk}"
        );

        let loaded = load_credentials_with(&store, &path).unwrap();
        assert_eq!(loaded, Some(creds));
    }

    #[test]
    fn file_fallback_when_keyring_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let store = MemStore::new(false);
        let creds = sample_creds();

        save_credentials_with(&store, &path, &creds).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("super-secret-token"),
            "token should fall back into the 0600 file when the keyring is down"
        );

        let loaded = load_credentials_with(&store, &path).unwrap();
        assert_eq!(loaded, Some(creds));
    }

    #[test]
    fn load_missing_credentials_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let store = MemStore::new(true);
        assert_eq!(load_credentials_with(&store, &path).unwrap(), None);
    }

    /// Manual integration check against a real server. Ignored by default; run with
    /// `JELLYFIN_URL`, `JELLYFIN_USER`, `JELLYFIN_PASS` set and `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires a live Jellyfin server"]
    async fn integration_real_server() {
        let (Ok(url), Ok(user), Ok(pass)) = (
            std::env::var("JELLYFIN_URL"),
            std::env::var("JELLYFIN_USER"),
            std::env::var("JELLYFIN_PASS"),
        ) else {
            eprintln!("skipping: set JELLYFIN_URL/USER/PASS to run");
            return;
        };

        let device_id = new_device_id();
        let creds = authenticate_by_name(&url, &device_id, &user, &pass)
            .await
            .expect("authenticate");
        let client = super::super::JellyfinClient::from_credentials(&creds).expect("client");

        let views = client.user_views().await.expect("user views");
        assert!(!views.is_empty(), "expected at least one library view");

        if let Some(first) = views.first() {
            let items = client
                .items(&super::super::models::ItemsQuery {
                    parent_id: Some(first.id.clone()),
                    limit: Some(1),
                    ..Default::default()
                })
                .await
                .expect("list items");
            if let Some(item) = items.items.first() {
                client.item(&item.id).await.expect("item detail");
            }
        }
    }
}
