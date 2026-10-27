//! Sideloadly remote-anisette auth. Two paths:
//!
//! **Anonymous (no login required)**
//! ```text
//! GET https://sideloadly.io/anisette/FzjicvV2PLvbwn14Bmj7s?u=<device_hash>
//! ```
//! It needs no caps exchange, browser, account, or installed Sideloadly session.
//! The device hash is a stable per-install ID stored locally.
//!
//! **Patreon (public instance)**
//! Open `/auth`, complete sign-in, then submit the returned Sideloadly link via
//! `complete_patreon_login`. `/caps?user_token=...` returns a capability JWT for
//! Patreon features; the anisette URL remains the shared remote endpoint.
//!
//! **OAuth (self-hosted instances supporting loopback redirects)**
//! ```text
//! open  https://<host>/auth?client_id=<id>&redirect_uri=<local>&scope=identity+campaigns
//!   -> the user signs in; the browser is redirected back to <local> with a token
//! GET   https://<host>/caps?user_token=<user_token>   -> { caps_token, ... }
//! GET   https://<host>/anisette/<caps_token>?u=<hash>
//! ```

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use plume_core::Error;
use plume_core::remote_anisette_v1::{DEFAULT_SIDELOADLY_HOST, sideloadly_anisette_url};
use plume_core::remote_servers::AnisetteServer;

/// Default Sideloadly OAuth client used for protocol compatibility. Users can
/// supply their own client ID for a self-hosted instance.
pub const DEFAULT_CLIENT_ID: &str =
    "fqo0L0ZdFYoKxshyuTdcQhZeQ8_E1XIjuI8i-CqIKIXM9wtuLAmhyNFlNhtt9XBN";

/// OAuth scope requested by the Sideloadly client.
pub const DEFAULT_SCOPE: &str = "identity campaigns";

/// Shared remote endpoint for anonymous anisette connections.
const ANONYMOUS_ANISETTE_URL: &str = "https://sideloadly.io/anisette/FzjicvV2PLvbwn14Bmj7s";

/// How long to wait for the browser to redirect back before giving up.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// Query-string keys a redirect might carry the session token under, in priority order.
const TOKEN_KEYS: [&str; 3] = ["user_token", "token", "code"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SideloadlyAccount {
    Anonymous,
    Patreon,
}

/// Our connection metadata; contains no Patreon user token or Sideloadly files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideloadlySession {
    pub account: SideloadlyAccount,
    pub server: AnisetteServer,
}

/// A Sideloadly instance and the OAuth client to authorize against it.
#[derive(Debug, Clone)]
pub struct SideloadlyAuth {
    /// Instance host, with or without a scheme (e.g. `sideloadly.io`).
    pub host: String,
    /// OAuth client id.
    pub client_id: String,
    /// OAuth scope.
    pub scope: String,
    /// Fixed redirect URI, when the client registers one. `None` uses a loopback
    /// callback server started for the login.
    pub redirect_uri: Option<String>,
}

impl Default for SideloadlyAuth {
    fn default() -> Self {
        Self {
            host: DEFAULT_SIDELOADLY_HOST.to_string(),
            client_id: DEFAULT_CLIENT_ID.to_string(),
            scope: DEFAULT_SCOPE.to_string(),
            redirect_uri: None,
        }
    }
}

impl SideloadlyAuth {
    /// Connects to Sideloadly's shared remote provider without OAuth.
    /// Only our per-install device hash is persisted; no Sideloadly files are read.
    pub async fn connect_anonymous(data_path: &std::path::Path) -> Result<AnisetteServer, Error> {
        let device_hash = plume_core::remote_anisette_v1::ensure_device_hash(data_path)
            .map_err(|e| Error::Sideloadly(format!("device hash: {e}")))?;
        let server = AnisetteServer::new(
            "Sideloadly",
            format!("{ANONYMOUS_ANISETTE_URL}?u={device_hash}"),
        );
        Self::check_connection(&server).await?;
        Ok(server)
    }

    /// Checks provider availability. This does not assert a Patreon membership tier.
    pub async fn check_connection(server: &AnisetteServer) -> Result<(), Error> {
        #[derive(Deserialize)]
        struct AnisetteResponse {
            #[serde(rename = "X-Apple-I-MD")]
            md: String,
            #[serde(rename = "X-Apple-I-MD-M")]
            md_m: String,
        }

        let headers = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .get(&server.address)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json::<AnisetteResponse>()
            .await?;
        if headers.md.is_empty() || headers.md_m.is_empty() {
            return Err(Error::Sideloadly(
                "the server returned empty anisette headers".into(),
            ));
        }
        Ok(())
    }

    /// Replaces `u` while retaining the provider URL and any unrelated parameters.
    /// The old identity is preserved if the new one cannot reach the provider.
    pub async fn refresh_device_token(
        data_path: &std::path::Path,
        server: &AnisetteServer,
    ) -> Result<AnisetteServer, Error> {
        let bytes = rand::random::<[u8; 32]>();
        let hash: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let refreshed = server_with_device_hash(server, &hash)?;
        Self::check_connection(&refreshed).await?;
        plume_core::remote_anisette_v1::save_device_hash(data_path, &hash)?;
        Ok(refreshed)
    }

    /// Public Sideloadly uses its registered Patreon callback, not a loopback URI.
    pub fn patreon_login_url(&self) -> String {
        format!("https://{}/auth", self.host())
    }

    /// Completes browser login from the returned Sideloadly link or user token.
    /// The capability JWT authenticates Patreon features; it is not an anisette URL.
    pub async fn complete_patreon_login(
        &self,
        data_path: &std::path::Path,
        completion: &str,
    ) -> Result<AnisetteServer, Error> {
        let token = token_from_completion(completion)
            .ok_or_else(|| Error::Sideloadly("paste the completed Sideloadly login link".into()))?;
        self.fetch_caps(&token).await?;
        let mut server = Self::connect_anonymous(data_path).await?;
        server.name = "Sideloadly (Patreon)".into();
        Ok(server)
    }

    /// Builds config from optional user input, falling back to the defaults for any
    /// blank field. A blank host targets the public instance; a blank client id uses
    /// the default client ID.
    pub fn new(host: &str, client_id: &str) -> Self {
        let pick = |value: &str, default: &str| {
            let value = value.trim();
            if value.is_empty() {
                default.to_string()
            } else {
                value.to_string()
            }
        };
        Self {
            host: pick(host, DEFAULT_SIDELOADLY_HOST),
            client_id: pick(client_id, DEFAULT_CLIENT_ID),
            scope: DEFAULT_SCOPE.to_string(),
            redirect_uri: None,
        }
    }

    /// Host with any scheme and trailing slash stripped.
    fn host(&self) -> &str {
        self.host
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_matches('/')
    }

    /// The URL to open in the browser to start the login.
    fn auth_url(&self, redirect_uri: &str) -> Result<String, Error> {
        reqwest::Url::parse_with_params(
            &format!("https://{}/auth", self.host()),
            &[
                ("client_id", self.client_id.trim()),
                ("redirect_uri", redirect_uri),
                ("scope", self.scope.as_str()),
            ],
        )
        .map(|u| u.to_string())
        .map_err(|e| Error::Sideloadly(format!("invalid auth URL: {e}")))
    }

    /// Exchanges the session token caught from the redirect for a capability token.
    async fn fetch_caps(&self, user_token: &str) -> Result<String, Error> {
        #[derive(Deserialize, Default)]
        struct CapsResponse {
            #[serde(default)]
            caps_token: String,
            #[serde(default)]
            error: String,
        }

        let url = reqwest::Url::parse_with_params(
            &format!("https://{}/caps", self.host()),
            &[("user_token", user_token)],
        )
        .map_err(|e| Error::Sideloadly(format!("invalid caps URL: {e}")))?;

        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| e.without_url())?;
        let status = response.status();
        let caps = response
            .json::<CapsResponse>()
            .await
            .map_err(|e| e.without_url())?;

        if !caps.error.is_empty() {
            return Err(Error::Sideloadly(caps.error));
        }
        if !status.is_success() {
            return Err(Error::Sideloadly(format!(
                "caps exchange returned HTTP {status}"
            )));
        }
        if caps.caps_token.is_empty() {
            return Err(Error::Sideloadly("no capability token was returned".into()));
        }
        Ok(caps.caps_token)
    }

    /// Runs the full OAuth login and returns the resulting anisette server. `open_url`
    /// is called with the authorization URL to open in the user's browser (kept out of
    /// this crate so the GUI owns how links are opened). The device hash is baked into
    /// the returned URL as `?u=<hash>` to match Sideloadly's sessions.json format.
    pub async fn login<F>(
        &self,
        data_path: &std::path::Path,
        open_url: F,
    ) -> Result<AnisetteServer, Error>
    where
        F: FnOnce(String),
    {
        if self.host() == DEFAULT_SIDELOADLY_HOST {
            return Err(Error::Sideloadly(
                "complete public Patreon login using the returned Sideloadly link".into(),
            ));
        }
        let device_hash = plume_core::remote_anisette_v1::ensure_device_hash(data_path)
            .map_err(|e| Error::Sideloadly(format!("device hash: {e}")))?;

        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let redirect_uri = self
            .redirect_uri
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}/callback"));

        let auth_url = self.auth_url(&redirect_uri)?;
        log::info!("Opening Sideloadly login at {auth_url}");
        open_url(auth_url);

        let user_token = tokio::task::spawn_blocking(move || wait_for_callback(listener))
            .await
            .map_err(|e| Error::Sideloadly(format!("callback task failed: {e}")))??;

        let caps_token = self.fetch_caps(&user_token).await?;
        let host = self.host();
        let url = format!(
            "{}?u={device_hash}",
            sideloadly_anisette_url(host, &caps_token)
        );
        Ok(AnisetteServer::new(format!("Sideloadly ({host})"), url))
    }
}

fn server_with_device_hash(server: &AnisetteServer, hash: &str) -> Result<AnisetteServer, Error> {
    let mut url = reqwest::Url::parse(&server.address)
        .map_err(|e| Error::Sideloadly(format!("invalid provider URL: {e}")))?;
    let params: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "u")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(params)
        .append_pair("u", hash);
    Ok(AnisetteServer::new(&server.name, url.to_string()))
}

fn token_from_completion(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.chars().any(char::is_whitespace) {
        return None;
    }
    if let Ok(url) = reqwest::Url::parse(input) {
        if let Some(token) = url.query().and_then(token_from_query) {
            return Some(token);
        }
        if let Some(token) = url.fragment().and_then(token_from_query) {
            return Some(token);
        }
        if url.scheme() == "sideloadly" {
            return url
                .path_segments()?
                .rfind(|p| !p.is_empty())
                .map(percent_decode);
        }
        return None;
    }
    if input.contains(['/', '?', '#', '=']) {
        return token_from_query(input.trim_start_matches('?'));
    }
    Some(input.to_string())
}

/// Serves the single loopback request the browser is redirected to, returning the
/// session token from its query string. Polls so it can time out without a live
/// connection to cancel it.
fn wait_for_callback(listener: TcpListener) -> Result<String, Error> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + CALLBACK_TIMEOUT;

    loop {
        match listener.accept() {
            Ok((stream, _)) => return handle_callback(stream),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(Error::Sideloadly(
                        "timed out waiting for the browser".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn handle_callback(stream: std::net::TcpStream) -> Result<String, Error> {
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    // "GET /callback?user_token=... HTTP/1.1"
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let token = token_from_query(query);

    let body = match &token {
        Some(_) => {
            "<html><body><h2>Signed in.</h2><p>You can close this tab and return to Jimpactor.</p></body></html>"
        }
        None => {
            "<html><body><h2>Login failed.</h2><p>No token was returned. You can close this tab.</p></body></html>"
        }
    };
    let mut stream = reader.into_inner();
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();

    token.ok_or_else(|| Error::Sideloadly("the login redirect carried no token".into()))
}

/// Finds and URL-decodes the session token in a query string.
fn token_from_query(query: &str) -> Option<String> {
    for expected in TOKEN_KEYS {
        for (key, value) in query.split('&').filter_map(|pair| pair.split_once('=')) {
            if key == expected && !value.is_empty() {
                return Some(percent_decode(value));
            }
        }
    }
    None
}

/// Minimal `application/x-www-form-urlencoded` decode (`+` → space, `%XX` → byte).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_blank_fields() {
        let auth = SideloadlyAuth::new("", "");
        assert_eq!(auth.host, DEFAULT_SIDELOADLY_HOST);
        assert_eq!(auth.client_id, DEFAULT_CLIENT_ID);

        let auth = SideloadlyAuth::new("ani.example.com", "my-client");
        assert_eq!(auth.host(), "ani.example.com");
        assert_eq!(auth.client_id, "my-client");
    }

    #[test]
    fn auth_url_carries_configured_client_and_redirect() {
        let auth = SideloadlyAuth::new("https://ani.example.com/", "my-client");
        let url = auth.auth_url("http://127.0.0.1:5000/callback").unwrap();
        assert!(url.starts_with("https://ani.example.com/auth?"));
        assert!(url.contains("client_id=my-client"));
        assert!(url.contains("redirect_uri=http"));
        assert!(url.contains("scope=identity"));
    }

    #[test]
    fn extracts_token_from_redirect_query() {
        assert_eq!(
            token_from_query("user_token=abc123&foo=bar").as_deref(),
            Some("abc123")
        );
        assert_eq!(token_from_query("code=a%2Bb").as_deref(), Some("a+b"));
        assert_eq!(token_from_query("state=x"), None);
        assert_eq!(token_from_query("unused&token=abc").as_deref(), Some("abc"));
    }

    #[test]
    fn completion_links_accept_the_returned_token() {
        assert_eq!(
            token_from_completion("sideloadly://auth?user_token=a%2Bb").as_deref(),
            Some("a+b")
        );
        assert_eq!(
            token_from_completion("sideloadly://auth/abc.def.ghi").as_deref(),
            Some("abc.def.ghi")
        );
        assert_eq!(
            token_from_completion("https://sideloadly.io/done#token=abc").as_deref(),
            Some("abc")
        );
        assert_eq!(
            token_from_completion("abc.def.ghi").as_deref(),
            Some("abc.def.ghi")
        );
        assert_eq!(token_from_completion("https://sideloadly.io/auth"), None);
        assert_eq!(token_from_completion("paste a link"), None);
    }

    fn mock_provider(body: &'static str) -> (AnisetteServer, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request = line.clone();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            request
        });
        (
            AnisetteServer::new(
                "test provider",
                format!("http://{address}/anisette/test?keep=yes&u=old&u=duplicate"),
            ),
            task,
        )
    }

    #[test]
    fn refresh_updates_the_saved_identity_and_preserves_the_provider() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jimpactor-refresh-{unique}"));
        let old = plume_core::remote_anisette_v1::ensure_device_hash(&path).unwrap();
        let (server, task) = mock_provider(r#"{"X-Apple-I-MD":"test","X-Apple-I-MD-M":"test"}"#);
        let refreshed = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(SideloadlyAuth::refresh_device_token(&path, &server))
            .unwrap();
        let hash = plume_core::remote_anisette_v1::ensure_device_hash(&path).unwrap();
        std::fs::remove_file(path.join("anisette_v1_device")).unwrap();
        std::fs::remove_dir(&path).unwrap();
        assert_ne!(old, hash);
        let url = reqwest::Url::parse(&refreshed.address).unwrap();
        let params: Vec<_> = url.query_pairs().collect();
        assert_eq!(params.iter().filter(|(key, _)| key == "u").count(), 1);
        assert!(
            params
                .iter()
                .any(|(key, value)| key == "u" && value == &hash)
        );
        assert!(
            params
                .iter()
                .any(|(key, value)| key == "keep" && value == "yes")
        );
        assert_eq!(refreshed.name, server.name);
        assert!(task.join().unwrap().contains(&hash));
    }

    #[test]
    fn failed_refresh_keeps_the_previous_identity() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jimpactor-refresh-failure-{unique}"));
        let old = plume_core::remote_anisette_v1::ensure_device_hash(&path).unwrap();
        let (server, task) = mock_provider(r#"{"X-Apple-I-MD":"test"}"#);
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(SideloadlyAuth::refresh_device_token(&path, &server));
        let hash = plume_core::remote_anisette_v1::ensure_device_hash(&path).unwrap();
        std::fs::remove_file(path.join("anisette_v1_device")).unwrap();
        std::fs::remove_dir(&path).unwrap();
        task.join().unwrap();
        assert!(result.is_err());
        assert_eq!(old, hash);
    }

    #[test]
    #[ignore = "network"]
    fn anonymous_connection_with_a_fresh_identity() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jimpactor-anonymous-{unique}"));
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(SideloadlyAuth::connect_anonymous(&path));
        let hash = plume_core::remote_anisette_v1::ensure_device_hash(&path).unwrap();
        let files = std::fs::read_dir(&path).unwrap().count();
        std::fs::remove_file(path.join("anisette_v1_device")).unwrap();
        std::fs::remove_dir(&path).unwrap();

        let server = result.expect("shared endpoint should return both anisette headers");
        assert_eq!(files, 1, "only our device identity should be persisted");
        assert_eq!(server.address, format!("{ANONYMOUS_ANISETTE_URL}?u={hash}"));
    }
}
