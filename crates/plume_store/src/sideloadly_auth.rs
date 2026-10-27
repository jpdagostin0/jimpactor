//! Live Sideloadly login: mints a remote-anisette capability token by running the
//! instance's two-stage authorization flow, so a token doesn't have to be pulled
//! out of an existing Sideloadly install.
//!
//! The flow, as used by Sideloadly's own client:
//! ```text
//! open  https://<host>/auth?client_id=<id>&redirect_uri=<local>&scope=identity+campaigns
//!   -> the user signs in; the browser is redirected back to <local> with a token
//! GET   https://<host>/caps?user_token=<user_token>   -> { caps_token, ... }
//! GET   https://<host>/anisette/<caps_token>?u=<hash>  (the v1 provider, elsewhere)
//! ```
//!
//! Both the host and the `client_id` are configurable: the defaults target the
//! public `sideloadly.io` instance, but a self-hoster can point this at their own
//! instance and OAuth client. The `redirect_uri` defaults to a loopback callback
//! server this module runs; an instance whose client registers a fixed redirect
//! can override it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use serde::Deserialize;

use plume_core::Error;
use plume_core::remote_anisette_v1::{DEFAULT_SIDELOADLY_HOST, sideloadly_anisette_url};
use plume_core::remote_servers::AnisetteServer;

/// Impactor's registered Sideloadly OAuth client. Used unless the user supplies
/// their own (e.g. for a self-hosted instance).
pub const DEFAULT_CLIENT_ID: &str =
    "fqo0L0ZdFYoKxshyuTdcQhZeQ8_E1XIjuI8i-CqIKIXM9wtuLAmhyNFlNhtt9XBN";

/// OAuth scope requested by the Sideloadly client.
pub const DEFAULT_SCOPE: &str = "identity campaigns";

/// How long to wait for the browser to redirect back before giving up.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// Query-string keys a redirect might carry the session token under, in priority order.
const TOKEN_KEYS: [&str; 3] = ["user_token", "token", "code"];

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
    /// Builds config from optional user input, falling back to the defaults for any
    /// blank field. A blank host targets the public instance; a blank client id uses
    /// Impactor's.
    pub fn new(host: &str, client_id: &str) -> Self {
        let pick = |value: &str, default: &str| {
            let value = value.trim();
            if value.is_empty() { default.to_string() } else { value.to_string() }
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

        let caps = reqwest::Client::new()
            .get(url)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json::<CapsResponse>()
            .await?;

        if !caps.error.is_empty() {
            return Err(Error::Sideloadly(caps.error));
        }
        if caps.caps_token.is_empty() {
            return Err(Error::Sideloadly("no capability token was returned".into()));
        }
        Ok(caps.caps_token)
    }

    /// Runs the full login and returns the resulting anisette server. `open_url` is
    /// called with the authorization URL to open in the user's browser (kept out of
    /// this crate so the GUI owns how links are opened).
    pub async fn login<F>(&self, open_url: F) -> Result<AnisetteServer, Error>
    where
        F: FnOnce(String),
    {
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
        Ok(AnisetteServer::new(
            format!("Sideloadly ({host})"),
            sideloadly_anisette_url(host, &caps_token),
        ))
    }
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
                    return Err(Error::Sideloadly("timed out waiting for the browser".into()));
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
        Some(_) => "<html><body><h2>Signed in.</h2><p>You can close this tab and return to Plume Impactor.</p></body></html>",
        None => "<html><body><h2>Login failed.</h2><p>No token was returned. You can close this tab.</p></body></html>",
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
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if TOKEN_KEYS.contains(&key) && !value.is_empty() {
            return Some(percent_decode(value));
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
        assert_eq!(token_from_query("user_token=abc123&foo=bar").as_deref(), Some("abc123"));
        assert_eq!(token_from_query("code=a%2Bb").as_deref(), Some("a+b"));
        assert_eq!(token_from_query("state=x"), None);
    }
}
